//! The text of the Linux `/proc` files, read into counters and gauges. Pure
//! functions: no I/O, no clock.

use std::collections::{BTreeMap, HashMap};

use super::proto::RawSample;

/// The `cpu` line of `/proc/stat`: time spent in each mode since boot, in
/// clock ticks, summed over all CPUs. `user` and `nice` include the guest
/// times, as the kernel counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
    pub guest: u64,
    pub guest_nice: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stat {
    pub cpu: CpuTimes,
    /// Number of `cpuN` lines: the CPUs online at the sample.
    pub cpus: u32,
    /// Context switches since boot.
    pub ctxt: Option<u64>,
    /// Processes and threads created since boot.
    pub processes: Option<u64>,
    pub procs_running: Option<u64>,
    pub procs_blocked: Option<u64>,
}

/// One device line of `/proc/diskstats`. Sectors are 512 bytes whatever the
/// device; times are milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DiskCounters {
    pub reads: u64,
    pub rd_sectors: u64,
    pub rd_ms: u64,
    pub writes: u64,
    pub wr_sectors: u64,
    pub wr_ms: u64,
    pub in_flight: u64,
    /// Time the device had at least one request in flight.
    pub io_ms: u64,
    /// Time in flight summed over the requests.
    pub weighted_ms: u64,
    /// Discards (Linux 4.18): completed, sectors.
    pub discards: Option<(u64, u64)>,
    /// Flush requests (Linux 5.5): completed, milliseconds spent.
    pub flushes: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LoadAvg {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

/// A `/proc/pressure/<resource>` file: total stall time in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pressure {
    pub some_us: u64,
    /// Absent from `cpu` before Linux 5.13.
    pub full_us: Option<u64>,
}

/// One sample, parsed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Seconds since boot, on the host's monotonic clock.
    pub uptime_s: f64,
    pub stat: Stat,
    /// `/proc/meminfo` by field name; kB for the sizes, counts for the
    /// `HugePages_*` fields.
    pub meminfo: HashMap<String, u64>,
    /// `/proc/vmstat` by counter name.
    pub vmstat: HashMap<String, u64>,
    /// Every line of `/proc/diskstats`, partitions included.
    pub disks: BTreeMap<String, DiskCounters>,
    pub loadavg: Option<LoadAvg>,
    pub pressure_cpu: Option<Pressure>,
    pub pressure_memory: Option<Pressure>,
    pub pressure_io: Option<Pressure>,
}

/// Parses a sample. The uptime, `stat` and `meminfo` are required; a host
/// without the other files gives a sample without their figures.
pub fn parse_sample(raw: &RawSample) -> Result<Parsed, String> {
    let required = |name: &str| {
        raw.file(name)
            .ok_or_else(|| format!("the sample has no /proc/{name}"))
    };
    Ok(Parsed {
        uptime_s: parse_uptime(required("uptime")?)?,
        stat: parse_stat(required("stat")?)?,
        meminfo: parse_key_values(required("meminfo")?),
        vmstat: raw.file("vmstat").map(parse_key_values).unwrap_or_default(),
        disks: raw
            .file("diskstats")
            .map(parse_diskstats)
            .unwrap_or_default(),
        loadavg: raw.file("loadavg").and_then(parse_loadavg),
        pressure_cpu: raw.file("pressure/cpu").and_then(parse_pressure),
        pressure_memory: raw.file("pressure/memory").and_then(parse_pressure),
        pressure_io: raw.file("pressure/io").and_then(parse_pressure),
    })
}

/// `/proc/uptime`: `<seconds since boot> <idle seconds>`.
pub fn parse_uptime(text: &str) -> Result<f64, String> {
    text.split_whitespace()
        .next()
        .and_then(|field| field.parse::<f64>().ok())
        .filter(|uptime| uptime.is_finite())
        .ok_or_else(|| format!("unreadable /proc/uptime: {:?}", text.trim()))
}

/// `/proc/stat`. Kernels before 2.6.33 have fewer than ten fields on the
/// `cpu` line; the missing ones are zero.
pub fn parse_stat(text: &str) -> Result<Stat, String> {
    let mut stat = Stat::default();
    let mut has_cpu = false;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else { continue };
        if key == "cpu" {
            let values: Vec<u64> = fields.map_while(|f| f.parse().ok()).collect();
            if values.len() < 4 {
                return Err(format!("unreadable cpu line in /proc/stat: {line:?}"));
            }
            let at = |i: usize| values.get(i).copied().unwrap_or(0);
            stat.cpu = CpuTimes {
                user: at(0),
                nice: at(1),
                system: at(2),
                idle: at(3),
                iowait: at(4),
                irq: at(5),
                softirq: at(6),
                steal: at(7),
                guest: at(8),
                guest_nice: at(9),
            };
            has_cpu = true;
        } else if key
            .strip_prefix("cpu")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            stat.cpus += 1;
        } else {
            let value = fields.next().and_then(|f| f.parse().ok());
            match key {
                "ctxt" => stat.ctxt = value,
                "processes" => stat.processes = value,
                "procs_running" => stat.procs_running = value,
                "procs_blocked" => stat.procs_blocked = value,
                _ => {}
            }
        }
    }
    if !has_cpu {
        return Err("/proc/stat has no cpu line".to_string());
    }
    Ok(stat)
}

/// `/proc/meminfo` (`MemTotal:  16314388 kB`) and `/proc/vmstat`
/// (`pgpgin 123`): one name and one integer per line.
pub fn parse_key_values(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let key = fields.next()?.trim_end_matches(':');
            let value = fields.next()?.parse().ok()?;
            Some((key.to_string(), value))
        })
        .collect()
}

/// `/proc/diskstats`: `major minor name` then 11 counters, 15 from Linux
/// 4.18 (discards) and 17 from 5.5 (flushes). Shorter lines are skipped.
pub fn parse_diskstats(text: &str) -> BTreeMap<String, DiskCounters> {
    let mut disks = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 14 {
            continue;
        }
        let values: Vec<u64> = fields[3..].iter().map_while(|f| f.parse().ok()).collect();
        if values.len() < 11 {
            continue;
        }
        disks.insert(
            fields[2].to_string(),
            DiskCounters {
                reads: values[0],
                rd_sectors: values[2],
                rd_ms: values[3],
                writes: values[4],
                wr_sectors: values[6],
                wr_ms: values[7],
                in_flight: values[8],
                io_ms: values[9],
                weighted_ms: values[10],
                discards: (values.len() >= 15).then(|| (values[11], values[13])),
                flushes: (values.len() >= 17).then(|| (values[15], values[16])),
            },
        );
    }
    disks
}

/// `/proc/loadavg`: `0.52 0.58 0.59 1/389 12345`.
pub fn parse_loadavg(text: &str) -> Option<LoadAvg> {
    let mut fields = text.split_whitespace().map(|f| f.parse::<f64>().ok());
    Some(LoadAvg {
        load1: fields.next()??,
        load5: fields.next()??,
        load15: fields.next()??,
    })
}

/// A pressure file: a `some` line and, for most resources, a `full` line,
/// each ending with `total=<microseconds>`.
pub fn parse_pressure(text: &str) -> Option<Pressure> {
    let total = |kind: &str| {
        text.lines()
            .find(|line| line.split_whitespace().next() == Some(kind))?
            .split_whitespace()
            .find_map(|field| field.strip_prefix("total="))?
            .parse::<u64>()
            .ok()
    };
    Some(Pressure {
        some_us: total("some")?,
        full_us: total("full"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Linux 6.1, two CPUs, the `intr` line already removed by the sampler.
    const STAT: &str = "\
cpu  451093 1024 152312 9731554 30121 0 4215 87 1200 30
cpu0 225100 512 76100 4865700 15060 0 2100 40 600 15
cpu1 225993 512 76212 4865854 15061 0 2115 47 600 15
ctxt 88120045
btime 1793000000
processes 310224
procs_running 3
procs_blocked 1
softirq 9120344 12 2301844 0 81223 0 0 310 3301221 0 3425734
";

    #[test]
    fn stat_gives_the_cpu_times_the_cpu_count_and_the_counters() {
        let stat = parse_stat(STAT).unwrap();
        assert_eq!(
            stat.cpu,
            CpuTimes {
                user: 451093,
                nice: 1024,
                system: 152312,
                idle: 9731554,
                iowait: 30121,
                irq: 0,
                softirq: 4215,
                steal: 87,
                guest: 1200,
                guest_nice: 30,
            }
        );
        assert_eq!(stat.cpus, 2);
        assert_eq!(stat.ctxt, Some(88120045));
        assert_eq!(stat.processes, Some(310224));
        assert_eq!(stat.procs_running, Some(3));
        assert_eq!(stat.procs_blocked, Some(1));
    }

    #[test]
    fn an_old_cpu_line_reads_its_missing_fields_as_zero() {
        // Linux 2.6.18: eight fields, no guest times.
        let stat =
            parse_stat("cpu  100 2 30 4000 50 6 7 8\ncpu0 100 2 30 4000 50 6 7 8\n").unwrap();
        assert_eq!(stat.cpu.steal, 8);
        assert_eq!(stat.cpu.guest, 0);
        assert_eq!(stat.cpu.guest_nice, 0);
        assert_eq!(stat.ctxt, None);
    }

    #[test]
    fn stat_without_a_cpu_line_is_an_error() {
        assert!(parse_stat("ctxt 12\n").is_err());
        assert!(parse_stat("cpu  1 2\n").is_err());
    }

    #[test]
    fn uptime_is_the_first_field() {
        assert_eq!(parse_uptime("351735.47 2792350.11\n"), Ok(351735.47));
        assert!(parse_uptime("\n").is_err());
        assert!(parse_uptime("nan 1.0\n").is_err());
    }

    #[test]
    fn meminfo_and_vmstat_share_a_parser() {
        let meminfo = parse_key_values(
            "MemTotal:       16314388 kB\nMemAvailable:    9876543 kB\nHugePages_Total:     512\nHugepagesize:       2048 kB\n",
        );
        assert_eq!(meminfo["MemTotal"], 16314388);
        assert_eq!(meminfo["MemAvailable"], 9876543);
        assert_eq!(meminfo["HugePages_Total"], 512);
        assert_eq!(meminfo["Hugepagesize"], 2048);

        let vmstat = parse_key_values("nr_free_pages 12345\npgpgin 777\npswpout 0\n");
        assert_eq!(vmstat["pgpgin"], 777);
        assert_eq!(vmstat["pswpout"], 0);
    }

    #[test]
    fn diskstats_accepts_the_three_line_lengths() {
        let text = "\
   8       0 sda 100 5 8000 250 200 10 16000 900 2 700 1150
 259       0 nvme0n1 100 5 8000 250 200 10 16000 900 2 700 1150 30 0 4096 12
 259       1 nvme0n1p1 10 0 800 25 20 1 1600 90 0 70 115 3 0 409 1 7 44
   7       0 loop0 1 0 2
";
        let disks = parse_diskstats(text);
        assert_eq!(disks.len(), 3, "the short loop0 line is skipped");

        let sda = disks["sda"];
        assert_eq!(
            (sda.reads, sda.rd_sectors, sda.rd_ms),
            (100, 8000, 250),
            "{sda:?}"
        );
        assert_eq!((sda.writes, sda.wr_sectors, sda.wr_ms), (200, 16000, 900));
        assert_eq!((sda.in_flight, sda.io_ms, sda.weighted_ms), (2, 700, 1150));
        assert_eq!(sda.discards, None);
        assert_eq!(sda.flushes, None);

        assert_eq!(disks["nvme0n1"].discards, Some((30, 4096)));
        assert_eq!(disks["nvme0n1"].flushes, None);
        assert_eq!(disks["nvme0n1p1"].discards, Some((3, 409)));
        assert_eq!(disks["nvme0n1p1"].flushes, Some((7, 44)));
    }

    #[test]
    fn loadavg_reads_the_three_averages() {
        assert_eq!(
            parse_loadavg("0.52 1.58 2.59 1/389 12345\n"),
            Some(LoadAvg {
                load1: 0.52,
                load5: 1.58,
                load15: 2.59
            })
        );
        assert_eq!(parse_loadavg("0.52\n"), None);
    }

    #[test]
    fn pressure_reads_the_totals_with_or_without_a_full_line() {
        let both = "some avg10=0.12 avg60=0.03 avg300=0.00 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=7890\n";
        assert_eq!(
            parse_pressure(both),
            Some(Pressure {
                some_us: 123456,
                full_us: Some(7890)
            })
        );
        let some_only = "some avg10=0.00 avg60=0.00 avg300=0.00 total=42\n";
        assert_eq!(
            parse_pressure(some_only),
            Some(Pressure {
                some_us: 42,
                full_us: None
            })
        );
        assert_eq!(parse_pressure(""), None);
    }

    #[test]
    fn a_sample_needs_uptime_stat_and_meminfo_only() {
        let raw = RawSample::from_files(&[
            ("uptime", "100.00 150.00\n"),
            ("stat", STAT),
            ("meminfo", "MemTotal: 1000 kB\n"),
        ]);
        let parsed = parse_sample(&raw).unwrap();
        assert_eq!(parsed.uptime_s, 100.0);
        assert!(parsed.vmstat.is_empty());
        assert!(parsed.disks.is_empty());
        assert_eq!(parsed.loadavg, None);
        assert_eq!(parsed.pressure_io, None);

        let no_stat = RawSample::from_files(&[("uptime", "100.00 150.00\n")]);
        let error = parse_sample(&no_stat).unwrap_err();
        assert!(error.contains("/proc/stat"), "{error}");
    }
}
