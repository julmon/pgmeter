//! The sampler's output stream, cut into samples. The format is described at
//! the top of `sampler.sh`.

use std::collections::{BTreeMap, HashMap};

/// The protocol version this decoder reads (`@hello <version>`).
pub const PROTOCOL_VERSION: u32 = 1;

/// What the sampler says about its host before the first sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hello {
    pub host: String,
    pub kernel: String,
    /// The host's IP addresses (`@addrs`).
    pub addrs: Vec<String>,
    /// Whole block devices (the entries of `/sys/block`); empty when the
    /// host does not expose them.
    pub blockdevs: Vec<String>,
    /// The whole devices under a device-mapper or RAID volume (`@stack`).
    pub stacks: BTreeMap<String, Vec<String>>,
    /// The mapper name of a device-mapper volume (`@dmname`): `vg-data`
    /// for `dm-3`.
    pub dm_names: BTreeMap<String, String>,
    /// What PostgreSQL keeps on a device (`@holds`): `("dm-3", "data")`.
    pub holds: Vec<(String, String)>,
}

/// One sample as text: the content of each `/proc` file, keyed by its path
/// under `/proc` (`stat`, `meminfo`, `pressure/io`).
#[derive(Debug, Default, PartialEq)]
pub struct RawSample {
    files: HashMap<String, String>,
}

impl RawSample {
    pub fn file(&self, name: &str) -> Option<&str> {
        self.files.get(name).map(String::as_str)
    }

    #[cfg(test)]
    pub fn from_files(files: &[(&str, &str)]) -> RawSample {
        RawSample {
            files: files
                .iter()
                .map(|(name, text)| (name.to_string(), text.to_string()))
                .collect(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// Once, when the first sample begins.
    Hello(Hello),
    /// A complete sample, on its `@end`.
    Sample(RawSample),
}

/// Line-by-line decoder of the sampler's stream.
#[derive(Default)]
pub struct Decoder {
    greeted: bool,
    hello: Hello,
    hello_sent: bool,
    current: Option<RawSample>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Takes one line of the stream, without or with its line ending.
    /// Returns an event when the line completes one, and an error when the
    /// sampler reports that it cannot run or speaks another protocol version.
    pub fn feed(&mut self, line: &str) -> Result<Option<Event>, String> {
        let line = line.trim_end_matches(['\r', '\n']);

        if let Some(message) = line.strip_prefix("@error ") {
            return Err(message.to_string());
        }

        // Whatever a login shell prints before the script starts is not ours.
        if !self.greeted {
            if let Some(version) = line.strip_prefix("@hello ") {
                if version.trim().parse() != Ok(PROTOCOL_VERSION) {
                    return Err(format!(
                        "the sampler speaks protocol {}, this pgmeter reads protocol {}",
                        version.trim(),
                        PROTOCOL_VERSION
                    ));
                }
                self.greeted = true;
            }
            return Ok(None);
        }

        if let Some(directive) = line.strip_prefix('@') {
            let (word, rest) = directive.split_once(' ').unwrap_or((directive, ""));
            match word {
                "host" => self.hello.host = rest.trim().to_string(),
                "kernel" => self.hello.kernel = rest.trim().to_string(),
                "blockdevs" => {
                    self.hello.blockdevs = rest.split_whitespace().map(str::to_string).collect()
                }
                "stack" => {
                    let mut words = rest.split_whitespace().map(str::to_string);
                    if let Some(device) = words.next() {
                        let mut under: Vec<String> = Vec::new();
                        for below in words {
                            // Two partitions of one disk name it twice.
                            if !under.contains(&below) {
                                under.push(below);
                            }
                        }
                        self.hello.stacks.insert(device, under);
                    }
                }
                "addrs" => self.hello.addrs = rest.split_whitespace().map(str::to_string).collect(),
                "holds" => {
                    if let Some((device, what)) = rest.trim().split_once(' ') {
                        self.hello
                            .holds
                            .push((device.to_string(), what.trim().to_string()));
                    }
                }
                "dmname" => {
                    if let Some((device, name)) = rest.trim().split_once(' ') {
                        self.hello
                            .dm_names
                            .insert(device.to_string(), name.trim().to_string());
                    }
                }
                "sample" => {
                    // A sample left open by a lost `@end` is dropped.
                    self.current = Some(RawSample::default());
                    if !self.hello_sent {
                        self.hello_sent = true;
                        return Ok(Some(Event::Hello(self.hello.clone())));
                    }
                }
                "end" => return Ok(self.current.take().map(Event::Sample)),
                // A directive added by a later sampler.
                _ => {}
            }
            return Ok(None);
        }

        if let Some(sample) = self.current.as_mut() {
            if let Some((path, content)) = line.split_once(':') {
                if let Some(name) = path.strip_prefix("/proc/") {
                    let text = sample.files.entry(name.to_string()).or_default();
                    text.push_str(content);
                    text.push('\n');
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(decoder: &mut Decoder, stream: &str) -> Vec<Event> {
        stream
            .lines()
            .filter_map(|line| decoder.feed(line).expect("a valid stream"))
            .collect()
    }

    const STREAM: &str = "\
Welcome to db1, authorized use only
@hello 1
@host db1
@kernel 6.1.0-48-amd64
@addrs 10.0.0.7 fd00::7
@blockdevs nvme0n1 sda dm-0
@holds dm-0 tablespace fast one
@dmname dm-0 vg-data
@stack dm-0 nvme0n1 nvme0n1
@sample
/proc/uptime:1000.00 7000.00
/proc/stat:cpu  10 0 5 100 1 0 0 0 0 0
/proc/stat:cpu0 10 0 5 100 1 0 0 0 0 0
/proc/pressure/io:some avg10=0.00 avg60=0.00 avg300=0.00 total=42
/proc/meminfo:MemTotal:       16314388 kB
@end
@sample
/proc/uptime:1001.00 7007.00
@end
";

    #[test]
    fn a_stream_decodes_into_a_hello_and_its_samples() {
        let mut decoder = Decoder::new();
        let events = feed_all(&mut decoder, STREAM);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0],
            Event::Hello(Hello {
                host: "db1".into(),
                kernel: "6.1.0-48-amd64".into(),
                addrs: vec!["10.0.0.7".into(), "fd00::7".into()],
                holds: vec![("dm-0".into(), "tablespace fast one".into())],
                blockdevs: vec!["nvme0n1".into(), "sda".into(), "dm-0".into()],
                stacks: BTreeMap::from([("dm-0".to_string(), vec!["nvme0n1".to_string()])]),
                dm_names: BTreeMap::from([("dm-0".to_string(), "vg-data".to_string())]),
            })
        );
        let Event::Sample(first) = &events[1] else {
            panic!("expected a sample, got {:?}", events[1]);
        };
        assert_eq!(first.file("uptime"), Some("1000.00 7000.00\n"));
        assert_eq!(
            first.file("stat"),
            Some("cpu  10 0 5 100 1 0 0 0 0 0\ncpu0 10 0 5 100 1 0 0 0 0 0\n")
        );
        // Only the first colon separates the path from the line.
        assert_eq!(first.file("meminfo"), Some("MemTotal:       16314388 kB\n"));
        assert_eq!(
            first.file("pressure/io"),
            Some("some avg10=0.00 avg60=0.00 avg300=0.00 total=42\n")
        );
        assert_eq!(first.file("vmstat"), None);
        let Event::Sample(second) = &events[2] else {
            panic!("expected a sample, got {:?}", events[2]);
        };
        assert_eq!(second.file("uptime"), Some("1001.00 7007.00\n"));
    }

    #[test]
    fn a_sample_cut_before_its_end_is_dropped() {
        let mut decoder = Decoder::new();
        let cut = "@hello 1\n@sample\n/proc/uptime:1.00 1.00\n@sample\n/proc/uptime:2.00 2.00\n@end\n/proc/uptime:9.00 9.00\n@end\n";
        let events = feed_all(&mut decoder, cut);
        // The hello, then the one sample that has both its marks; the stray
        // line and the second `@end` belong to no sample.
        assert_eq!(events.len(), 2);
        let Event::Sample(sample) = &events[1] else {
            panic!("expected a sample, got {:?}", events[1]);
        };
        assert_eq!(sample.file("uptime"), Some("2.00 2.00\n"));
    }

    #[test]
    fn the_sampler_error_and_a_version_mismatch_are_errors() {
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.feed("@error unsupported operating system: Darwin"),
            Err("unsupported operating system: Darwin".to_string())
        );
        let mut decoder = Decoder::new();
        let error = decoder.feed("@hello 2").unwrap_err();
        assert!(error.contains("protocol 2"), "{error}");
    }

    #[test]
    fn line_endings_and_unknown_directives_are_tolerated() {
        let mut decoder = Decoder::new();
        let stream = "@hello 1\r\n@host db1\r\n@future thing\r\n@sample\r\n/proc/loadavg:0.10 0.20 0.30 1/100 4242\r\n@end\r\n";
        let events = feed_all(&mut decoder, stream);
        assert_eq!(events.len(), 2);
        let Event::Sample(sample) = &events[1] else {
            panic!("expected a sample, got {:?}", events[1]);
        };
        assert_eq!(sample.file("loadavg"), Some("0.10 0.20 0.30 1/100 4242\n"));
    }
}
