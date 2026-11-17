//! A host to monitor, as given to `--sysmon`, and the child process that
//! runs the sampler there.

use std::fmt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// The environment variable that replaces the `ssh` program. It may carry
/// arguments, separated by blanks: `PGMETER_SSH="ssh -F ./ssh_config"`.
pub const SSH_ENV: &str = "PGMETER_SSH";

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// The machine pgmeter runs on.
    Local,
    /// A host reached with the system's `ssh`: `[user@]host`, an alias of
    /// `~/.ssh/config`, or an `ssh://` URI.
    Ssh {
        destination: String,
        port: Option<u16>,
    },
}

/// A `--sysmon` value: a target, marked or not as the database server.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub target: Target,
    /// Given as `db:<target>`: this host is the database server, whatever
    /// pgmeter would have worked out.
    pub database: bool,
}

impl Spec {
    /// `[db:]local` or `[db:][user@]host[:port]`. `db:5432` stays what it
    /// was before the marker existed: the host `db` on port 5432.
    pub fn parse(value: &str) -> Result<Spec, String> {
        match value.strip_prefix("db:") {
            Some(rest) if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) => {
                Ok(Spec {
                    target: Target::parse(rest)?,
                    database: true,
                })
            }
            _ => Ok(Spec {
                target: Target::parse(value)?,
                database: false,
            }),
        }
    }
}

impl Target {
    /// Reads a target: `local` or `[user@]host[:port]`.
    pub fn parse(value: &str) -> Result<Target, String> {
        if value == "local" {
            return Ok(Target::Local);
        }
        if value.is_empty() {
            return Err("empty target".to_string());
        }
        // The destination is an argument of `ssh`: it must not read as one of
        // its options, whatever comes before it on the command line.
        if value.starts_with('-') {
            return Err("an SSH destination cannot start with '-'".to_string());
        }
        if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("an SSH destination cannot contain blanks".to_string());
        }
        // ssh reads the port of a URI itself.
        if value.starts_with("ssh://") {
            return Ok(Target::Ssh {
                destination: value.to_string(),
                port: None,
            });
        }

        let (user, host_port) = match value.rsplit_once('@') {
            Some((user, rest)) => (Some(user), rest),
            None => (None, value),
        };
        let (host, port) = split_port(host_port)?;
        if host.is_empty() || host.starts_with('-') || user.is_some_and(str::is_empty) {
            return Err(format!("{value:?} is not [user@]host[:port]"));
        }
        let destination = match user {
            Some(user) => format!("{user}@{host}"),
            None => host.to_string(),
        };
        Ok(Target::Ssh { destination, port })
    }

    /// The child process that runs the sampler: `sh` here, or `ssh` to a
    /// `sh` there. It reads the script on its standard input and takes the
    /// interval as the script's first argument. `ssh_program` is the SSH
    /// client and its leading arguments.
    ///
    /// The child gets its own process group, so a Ctrl-C at the terminal,
    /// which pgmeter handles by letting the clients finish, does not kill it.
    pub fn command(&self, interval_s: f64, ssh_program: &[String]) -> Command {
        let mut command = match self {
            Target::Local => Command::new("sh"),
            Target::Ssh { destination, port } => {
                let (program, leading) = ssh_program
                    .split_first()
                    .expect("the SSH program is never empty");
                let mut command = Command::new(program);
                command.args(leading).args([
                    // No terminal, and no prompt: without a key or an agent
                    // the connection fails at once instead of waiting for a
                    // password nobody is asked for.
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=10",
                    // A dead link ends the child within 45 s.
                    "-o",
                    "ServerAliveInterval=15",
                    "-o",
                    "ServerAliveCountMax=3",
                ]);
                if let Some(port) = port {
                    command.arg("-p").arg(port.to_string());
                }
                command.arg("--").arg(destination).arg("sh");
                command
            }
        };
        command
            .args(["-s", "--", &interval_s.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Local => f.write_str("local"),
            Target::Ssh {
                destination,
                port: Some(port),
            } => write!(f, "{destination}:{port}"),
            Target::Ssh {
                destination,
                port: None,
            } => f.write_str(destination),
        }
    }
}

/// `host`, `host:port`, `[v6]`, `[v6]:port`, or a bare IPv6 address, which
/// has colons but no port.
fn split_port(host_port: &str) -> Result<(&str, Option<u16>), String> {
    let parse_port = |port: &str| {
        port.parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("{port:?} is not a port number"))
    };
    if let Some(rest) = host_port.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("missing ']' in {host_port:?}"))?;
        return match after.strip_prefix(':') {
            Some(port) => Ok((host, Some(parse_port(port)?))),
            None if after.is_empty() => Ok((host, None)),
            None => Err(format!("unexpected text after ']' in {host_port:?}")),
        };
    }
    match host_port.split_once(':') {
        Some((_, rest)) if rest.contains(':') => Ok((host_port, None)),
        Some((host, port)) => Ok((host, Some(parse_port(port)?))),
        None => Ok((host_port, None)),
    }
}

/// The SSH client to run: `PGMETER_SSH` split on blanks, or `ssh`.
pub fn ssh_program() -> Vec<String> {
    let words: Vec<String> = std::env::var(SSH_ENV)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        vec!["ssh".to_string()]
    } else {
        words
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(destination: &str, port: Option<u16>) -> Target {
        Target::Ssh {
            destination: destination.to_string(),
            port,
        }
    }

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn targets_parse_and_print_back() {
        for (value, target) in [
            ("local", Target::Local),
            ("db1", ssh("db1", None)),
            ("postgres@db1", ssh("postgres@db1", None)),
            (
                "postgres@db1.example.com:2222",
                ssh("postgres@db1.example.com", Some(2222)),
            ),
            ("10.0.0.7:22", ssh("10.0.0.7", Some(22))),
            ("fe80::1", ssh("fe80::1", None)),
            (
                "ssh://postgres@db1:2222",
                ssh("ssh://postgres@db1:2222", None),
            ),
        ] {
            let parsed = Target::parse(value).unwrap();
            assert_eq!(parsed, target, "{value}");
            assert_eq!(parsed.to_string(), value);
        }
        assert_eq!(
            Target::parse("root@[fe80::1]:2222"),
            Ok(ssh("root@fe80::1", Some(2222)))
        );
        assert_eq!(Target::parse("[::1]"), Ok(ssh("::1", None)));
    }

    #[test]
    fn the_database_marker_is_read_off_the_target() {
        let spec = Spec::parse("db:postgres@db1:2222").unwrap();
        assert!(spec.database);
        assert_eq!(spec.target, ssh("postgres@db1", Some(2222)));
        let spec = Spec::parse("db:local").unwrap();
        assert!(spec.database && spec.target == Target::Local);
        let spec = Spec::parse("postgres@db1").unwrap();
        assert!(!spec.database);
        // A host named `db`, with its port.
        let spec = Spec::parse("db:2222").unwrap();
        assert!(!spec.database);
        assert_eq!(spec.target, ssh("db", Some(2222)));
        // The marker changes nothing to what a target may be.
        assert!(Spec::parse("db:").is_err());
        assert!(Spec::parse("db:-oProxyCommand=id").is_err());
    }

    #[test]
    fn what_could_read_as_an_ssh_option_is_refused() {
        for value in [
            "",
            "-oProxyCommand=touch /tmp/x",
            "-oProxyCommand=id",
            "user@-oProxyCommand=id",
            "db1 -oProxyCommand=id",
            "db1\tx",
            "@db1",
            "user@",
            "db1:",
            "db1:0",
            "db1:ssh",
            "db1:70000",
            "[::1",
            "[::1]x",
        ] {
            assert!(Target::parse(value).is_err(), "{value:?} was accepted");
        }
    }

    #[test]
    fn the_local_sampler_is_a_shell_reading_its_script() {
        let command = Target::Local.command(1.0, &["ssh".to_string()]);
        assert_eq!(argv(&command), ["sh", "-s", "--", "1"]);
    }

    #[test]
    fn the_remote_sampler_goes_through_ssh_without_a_prompt() {
        let program = ["ssh".to_string(), "-F".to_string(), "cfg".to_string()];
        let command = ssh("postgres@db1", Some(2222)).command(5.0, &program);
        assert_eq!(
            argv(&command),
            [
                "ssh",
                "-F",
                "cfg",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-p",
                "2222",
                "--",
                "postgres@db1",
                "sh",
                "-s",
                "--",
                "5"
            ]
        );
        let command = ssh("db1", None).command(0.5, &program[..1]);
        let args = argv(&command);
        assert!(!args.contains(&"-p".to_string()));
        assert_eq!(args.last().unwrap(), "0.5");
    }
}
