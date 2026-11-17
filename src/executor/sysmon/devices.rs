//! How a host's block devices stack: a logical volume on an encrypted
//! volume on a disk, a RAID array on its members, a multipath device on its
//! paths. Every layer has its line in `/proc/diskstats` and counts the same
//! I/O again; the top of a stack is the device a file system sits on, the
//! one PostgreSQL waits for.

use super::delta::is_pseudo;
use super::proto::Hello;

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// The kernel name, as in `/proc/diskstats` and the disk CSV file.
    pub name: String,
    /// The mapper name of a device-mapper volume: `vg-data` for `dm-3`.
    pub mapper_name: Option<String>,
    /// The whole devices directly under this one; empty for a disk.
    pub on: Vec<String>,
    /// No other device sits on this one.
    pub top: bool,
    /// What PostgreSQL keeps on it: `data`, `WAL`, `tablespace <name>`.
    /// Known on the database server only, and only when pgmeter may read
    /// the server's data directory setting.
    pub holds: Vec<String>,
}

impl Device {
    /// The device's entry in `run.json`.
    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "mapper_name": self.mapper_name,
            "on": self.on,
            "top": self.top,
            "holds": self.holds,
        })
    }
}

/// The devices a host listed, without the loop and RAM ones, in its order.
/// `database` says whether this host is the database server: the paths of
/// the server are looked up on every host, and mean something on that one.
pub fn topology(hello: &Hello, database: bool) -> Vec<Device> {
    let listed = |name: &String| !is_pseudo(name) && hello.blockdevs.contains(name);
    let on_of = |name: &String| -> Vec<String> {
        hello
            .stacks
            .get(name)
            .map(|under| under.iter().filter(|b| listed(b)).cloned().collect())
            .unwrap_or_default()
    };
    hello
        .blockdevs
        .iter()
        .filter(|name| !is_pseudo(name))
        .map(|name| Device {
            name: name.clone(),
            mapper_name: hello.dm_names.get(name).cloned(),
            on: on_of(name),
            top: !hello
                .blockdevs
                .iter()
                .any(|other| listed(other) && on_of(other).contains(name)),
            holds: hello
                .holds
                .iter()
                .filter(|(device, _)| database && device == name)
                .map(|(_, what)| what.clone())
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::sysmon::proto::{Decoder, Event};

    fn hello(header: &str) -> Hello {
        let mut decoder = Decoder::new();
        let stream = format!("@hello 1\n{header}@sample\n");
        for line in stream.lines() {
            if let Some(Event::Hello(hello)) = decoder.feed(line).unwrap() {
                return hello;
            }
        }
        panic!("no hello in {stream:?}");
    }

    fn topo(header: &str) -> Vec<Device> {
        topology(&hello(header), false)
    }

    fn tops(devices: &[Device]) -> Vec<&str> {
        devices
            .iter()
            .filter(|d| d.top)
            .map(|d| d.name.as_str())
            .collect()
    }

    #[test]
    fn a_logical_volume_on_an_encrypted_disk_is_the_top_of_its_stack() {
        let devices = topo(
            "@blockdevs dm-0 dm-1 dm-2 loop0 nvme0n1\n\
             @dmname dm-0 nvme0n1p3_crypt\n@stack dm-0 nvme0n1\n\
             @dmname dm-1 vg-root\n@stack dm-1 dm-0\n\
             @dmname dm-2 vg-swap_1\n@stack dm-2 dm-0\n",
        );
        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["dm-0", "dm-1", "dm-2", "nvme0n1"]);
        assert_eq!(tops(&devices), ["dm-1", "dm-2"]);
        assert_eq!(devices[1].mapper_name.as_deref(), Some("vg-root"));
        assert_eq!(devices[1].on, ["dm-0"]);
        assert_eq!(devices[3].mapper_name, None);
        assert!(devices[3].on.is_empty());
    }

    #[test]
    fn a_multipath_device_is_the_top_and_its_paths_are_not() {
        let devices =
            topo("@blockdevs sda sdb sdc dm-0\n@dmname dm-0 mpatha\n@stack dm-0 sdb sdc\n");
        assert_eq!(tops(&devices), ["sda", "dm-0"]);
    }

    #[test]
    fn what_postgresql_keeps_is_labelled_on_the_database_server_only() {
        let header = "@blockdevs dm-0 dm-1 nvme0n1\n@stack dm-0 nvme0n1\n@stack dm-1 nvme0n1\n\
                      @holds dm-0 data\n@holds dm-1 WAL\n@holds dm-0 tablespace fast\n";
        let devices = topology(&hello(header), true);
        assert_eq!(devices[0].holds, ["data", "tablespace fast"]);
        assert_eq!(devices[1].holds, ["WAL"]);
        assert!(devices[2].holds.is_empty());
        // The same paths exist on a replica: it is not the server under test.
        let devices = topology(&hello(header), false);
        assert!(devices.iter().all(|d| d.holds.is_empty()));
    }

    #[test]
    fn a_host_that_reports_no_stack_has_every_device_on_top() {
        let devices = topo("@blockdevs sda nvme0n1 ram0\n");
        assert_eq!(tops(&devices), ["sda", "nvme0n1"]);
        // A stack that names a device the host did not list is not followed.
        let devices = topo("@blockdevs md0\n@stack md0 sdx\n");
        assert!(devices[0].on.is_empty());
        assert_eq!(
            devices[0].metadata(),
            serde_json::json!({"name": "md0", "mapper_name": null, "on": [], "top": true, "holds": []})
        );
    }
}
