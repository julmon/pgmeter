//! Which monitored host is the database server, and where PostgreSQL keeps
//! its files there. Monitoring itself is never implied by any of this: it
//! only tells apart the hosts that `--sysmon` names.

use std::net::IpAddr;

use postgres::Client;

use super::proto::Hello;
use super::target::Target;

/// Where PostgreSQL accepted pgmeter's connection.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ServerAddr {
    /// The question was not asked, or got no usable answer.
    #[default]
    Unknown,
    /// A Unix socket or a loopback address: the server is on this machine.
    Local,
    Remote(IpAddr),
}

/// What pgmeter knows of the database server when the samplers start.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Database {
    /// The index, among the targets, of the one given as `db:<target>`.
    pub marked: Option<usize>,
    pub addr: ServerAddr,
    /// What PostgreSQL keeps where: `("data", "/var/lib/postgresql/18/main")`,
    /// `("WAL", ".../pg_wal")`, `("tablespace fast", "/mnt/fast")`.
    pub paths: Vec<(String, String)>,
}

impl Database {
    /// Asks the server where it listens and where its files are. A role
    /// that may not read `data_directory` (it takes a superuser or a member
    /// of `pg_read_all_settings`) gets no path, and so no volume label.
    pub fn lookup(client: &mut Client, marked: Option<usize>) -> Database {
        let addr = match client.query_one("SELECT host(inet_server_addr())", &[]) {
            Ok(row) => match row.get::<_, Option<String>>(0) {
                // No address: a Unix socket.
                None => ServerAddr::Local,
                Some(text) => match text.parse::<IpAddr>() {
                    Ok(ip) if ip.is_loopback() => ServerAddr::Local,
                    Ok(ip) => ServerAddr::Remote(ip),
                    Err(_) => ServerAddr::Unknown,
                },
            },
            Err(_) => ServerAddr::Unknown,
        };

        let mut paths = Vec::new();
        let data_directory = client
            .query_one("SELECT current_setting('data_directory')", &[])
            .ok()
            .and_then(|row| row.get::<_, Option<String>>(0));
        if let Some(data_directory) = data_directory {
            // There is no setting for the WAL: it is this directory, often a
            // symbolic link to another disk.
            paths.push(("WAL".to_string(), format!("{data_directory}/pg_wal")));
            paths.push(("data".to_string(), data_directory));
            let tablespaces = client.query(
                "SELECT spcname::text, pg_tablespace_location(oid) FROM pg_tablespace \
                 WHERE pg_tablespace_location(oid) <> '' ORDER BY spcname",
                &[],
            );
            for row in tablespaces.unwrap_or_default() {
                let name: String = row.get(0);
                paths.push((format!("tablespace {name}"), row.get(1)));
            }
        }
        // A label or a path with a line break would not fit the sampler's
        // line protocol.
        paths.retain(|(label, path)| !label.chars().chain(path.chars()).any(char::is_control));
        paths.sort_by_key(|(label, _)| match label.as_str() {
            "data" => 0,
            "WAL" => 1,
            _ => 2,
        });

        Database {
            marked,
            addr,
            paths,
        }
    }

    /// The index of the database server among the hosts that answered: the
    /// one marked `db:`; else the one PostgreSQL's address belongs to; else
    /// the first, which is a guess (a pooler or NAT in between).
    pub fn host(&self, targets: &[Target], hellos: &[&Hello]) -> usize {
        if let Some(marked) = self.marked.filter(|index| *index < targets.len()) {
            return marked;
        }
        let found = match &self.addr {
            ServerAddr::Local => targets.iter().position(|t| *t == Target::Local),
            ServerAddr::Remote(addr) => hellos.iter().position(|hello| {
                hello
                    .addrs
                    .iter()
                    .any(|text| text.parse::<IpAddr>().is_ok_and(|ip| ip == *addr))
            }),
            ServerAddr::Unknown => None,
        };
        found.unwrap_or(0)
    }

    /// What goes ahead of the sampler script: the paths to look up, as a
    /// shell function the script calls. The paths come from the server and
    /// reach a shell on the host, so each is a single-quoted word; nothing
    /// of them ever goes on a command line.
    pub fn script_prefix(&self) -> String {
        if self.paths.is_empty() {
            return String::new();
        }
        let mut prefix = String::from("pgm_paths() {\n");
        for (label, path) in &self.paths {
            prefix.push_str(&format!("    holds {} {}\n", quote(label), quote(path)));
        }
        prefix.push_str("}\n");
        prefix
    }
}

/// A word for `sh`: single-quoted, with each quote of the text closed,
/// escaped and reopened.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn ssh(destination: &str) -> Target {
        Target::Ssh {
            destination: destination.to_string(),
            port: None,
        }
    }

    fn hello(addrs: &[&str]) -> Hello {
        Hello {
            addrs: addrs.iter().map(|a| a.to_string()).collect(),
            ..Hello::default()
        }
    }

    #[test]
    fn the_marker_wins_then_the_address_then_the_first_target() {
        let targets = [ssh("app1"), Target::Local, ssh("db1")];
        let app = hello(&["10.0.0.5"]);
        let local = hello(&["10.0.0.9", "fd00::9"]);
        let db = hello(&["10.0.0.7", "fd00::7"]);
        let hellos = [&app, &local, &db];
        let database = |marked, addr| Database {
            marked,
            addr,
            paths: Vec::new(),
        };
        let remote = |text: &str| ServerAddr::Remote(text.parse().unwrap());

        assert_eq!(
            database(None, remote("10.0.0.7")).host(&targets, &hellos),
            2
        );
        // The same address written another way is the same address.
        assert_eq!(
            database(None, remote("fd00:0::7")).host(&targets, &hellos),
            2
        );
        assert_eq!(database(None, ServerAddr::Local).host(&targets, &hellos), 1);
        // The marker is not argued with.
        assert_eq!(
            database(Some(0), remote("10.0.0.7")).host(&targets, &hellos),
            0
        );
        // Behind a pooler the address is nobody's: the first target.
        assert_eq!(
            database(None, remote("192.0.2.1")).host(&targets, &hellos),
            0
        );
        assert_eq!(
            database(None, ServerAddr::Unknown).host(&targets, &hellos),
            0
        );
        // A local server that is not monitored: the first target as well.
        let remote_only = [ssh("app1")];
        assert_eq!(
            database(None, ServerAddr::Local).host(&remote_only, &[&app]),
            0
        );
    }

    #[test]
    fn no_path_means_no_prefix() {
        assert_eq!(Database::default().script_prefix(), "");
    }

    // The quoting is checked by the shell itself: whatever the path holds,
    // the function hands it over as one unchanged word and runs nothing.
    #[test]
    fn a_hostile_path_reaches_the_shell_as_one_word() {
        let dir = tempfile::tempdir().unwrap();
        let canary = dir.path().join("canary");
        let hostile = format!(
            "/pg data/it's; touch {c} $(touch {c}) `touch {c}` \"x\" \\",
            c = canary.display()
        );
        let database = Database {
            paths: vec![
                ("data".to_string(), hostile.clone()),
                ("tablespace o'neil; id".to_string(), "/mnt/ts".to_string()),
            ],
            ..Database::default()
        };
        let script = format!(
            "holds() {{ printf '%s|%s\\n' \"$1\" \"$2\"; }}\n{}pgm_paths\n",
            database.script_prefix()
        );
        let mut child = Command::new("sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("data|{hostile}\ntablespace o'neil; id|/mnt/ts\n")
        );
        assert!(!canary.exists(), "the path was run as a command");
    }
}
