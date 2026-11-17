//! Two consecutive parsed samples turned into the figures of the interval
//! between them: shares of CPU time, rates per second, and the gauges as
//! they stood at the second sample.

use super::parse::{CpuTimes, DiskCounters, Parsed, Pressure};

/// The `/proc/meminfo` fields kept, in the order of the output columns. kB,
/// except the `HugePages_*` counts.
pub const MEM_GAUGES: [&str; 24] = [
    "MemTotal",
    "MemFree",
    "MemAvailable",
    "Buffers",
    "Cached",
    "Shmem",
    "AnonPages",
    "Mapped",
    "Dirty",
    "Writeback",
    "Slab",
    "SReclaimable",
    "SUnreclaim",
    "PageTables",
    "KernelStack",
    "SwapTotal",
    "SwapFree",
    "SwapCached",
    "CommitLimit",
    "Committed_AS",
    "HugePages_Total",
    "HugePages_Free",
    "HugePages_Rsvd",
    "Hugepagesize",
];

/// The `/proc/vmstat` counters kept, as rates per second. `pgpgin` and
/// `pgpgout` are kB, the swap and fault counters are pages or events.
pub const VM_RATES: [&str; 7] = [
    "pgpgin",
    "pgpgout",
    "pswpin",
    "pswpout",
    "pgfault",
    "pgmajfault",
    "oom_kill",
];

/// CPU time of the interval by mode, in percent of all CPUs together; the
/// ten shares add up to 100. `user` and `nice` exclude the guest times,
/// which have their own shares.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuShares {
    pub user: f64,
    pub nice: f64,
    pub system: f64,
    pub iowait: f64,
    pub irq: f64,
    pub softirq: f64,
    pub steal: f64,
    pub guest: f64,
    pub guest_nice: f64,
    pub idle: f64,
}

impl CpuShares {
    pub fn total(&self) -> f64 {
        self.user
            + self.nice
            + self.system
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
            + self.guest
            + self.guest_nice
            + self.idle
    }

    /// Everything but idle and iowait: the share of time a CPU was executing.
    pub fn busy(&self) -> f64 {
        self.total() - self.idle - self.iowait
    }
}

/// Scheduler activity: rates over the interval and gauges at its end.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Activity {
    pub ctxt_s: Option<f64>,
    pub forks_s: Option<f64>,
    pub procs_running: Option<u64>,
    pub procs_blocked: Option<u64>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
}

/// Share of the interval, in percent, during which some or all runnable
/// tasks were stalled on a resource.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PressureShares {
    pub some: Option<f64>,
    pub full: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySample {
    /// One value per `MEM_GAUGES` entry; None when the kernel has no such field.
    pub gauges: Vec<Option<u64>>,
    /// One value per `VM_RATES` entry, per second.
    pub rates: Vec<Option<f64>>,
}

impl MemorySample {
    pub fn gauge(&self, name: &str) -> Option<u64> {
        let index = MEM_GAUGES.iter().position(|field| *field == name)?;
        self.gauges.get(index).copied().flatten()
    }

    pub fn rate(&self, name: &str) -> Option<f64> {
        let index = VM_RATES.iter().position(|field| *field == name)?;
        self.rates.get(index).copied().flatten()
    }
}

/// One block device over the interval: the figures of `iostat -x`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskSample {
    pub device: String,
    /// Reads and writes completed per second.
    pub r_s: f64,
    pub w_s: f64,
    /// kB read and written per second.
    pub rkb_s: f64,
    pub wkb_s: f64,
    /// Average time of a read or a write, queueing included; 0 without any.
    pub r_await_ms: f64,
    pub w_await_ms: f64,
    /// Average size of a read or a write; 0 without any.
    pub rareq_kb: f64,
    pub wareq_kb: f64,
    /// Average number of requests in flight.
    pub aqu_sz: f64,
    /// Share of the interval with at least one request in flight.
    pub util_pct: f64,
    /// Discards per second and kB discarded per second, where reported.
    pub d_s: Option<f64>,
    pub dkb_s: Option<f64>,
    /// Flush requests per second, where reported.
    pub f_s: Option<f64>,
    /// Average time of a flush, where reported; 0 without any.
    pub f_await_ms: Option<f64>,
}

/// The figures of one interval.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    /// Length of the interval on the host's clock, in seconds.
    pub interval_s: f64,
    /// None when no CPU time was accounted over the interval.
    pub cpu: Option<CpuShares>,
    pub activity: Activity,
    pub pressure_cpu: PressureShares,
    pub pressure_memory: PressureShares,
    pub pressure_io: PressureShares,
    pub memory: MemorySample,
    /// The monitored devices, by name.
    pub disks: Vec<DiskSample>,
}

/// The figures of the interval from `prev` to `cur`. `blockdevs` is the
/// host's list of whole block devices. None when the host's clock did not
/// advance between the two samples.
pub fn delta(prev: &Parsed, cur: &Parsed, blockdevs: &[String]) -> Option<Sample> {
    let interval_s = cur.uptime_s - prev.uptime_s;
    if !interval_s.is_finite() || interval_s <= 0.0 {
        return None;
    }

    let disks = cur
        .disks
        .iter()
        .filter(|(name, counters)| is_monitored(name, counters, blockdevs))
        .filter_map(|(name, counters)| {
            disk_sample(name, prev.disks.get(name)?, counters, interval_s)
        })
        .collect();

    Some(Sample {
        interval_s,
        cpu: cpu_shares(&prev.stat.cpu, &cur.stat.cpu),
        activity: Activity {
            ctxt_s: rate(prev.stat.ctxt, cur.stat.ctxt, interval_s),
            forks_s: rate(prev.stat.processes, cur.stat.processes, interval_s),
            procs_running: cur.stat.procs_running,
            procs_blocked: cur.stat.procs_blocked,
            load1: cur.loadavg.map(|l| l.load1),
            load5: cur.loadavg.map(|l| l.load5),
            load15: cur.loadavg.map(|l| l.load15),
        },
        pressure_cpu: pressure_shares(prev.pressure_cpu, cur.pressure_cpu, interval_s),
        pressure_memory: pressure_shares(prev.pressure_memory, cur.pressure_memory, interval_s),
        pressure_io: pressure_shares(prev.pressure_io, cur.pressure_io, interval_s),
        memory: MemorySample {
            gauges: MEM_GAUGES
                .iter()
                .map(|field| cur.meminfo.get(*field).copied())
                .collect(),
            rates: VM_RATES
                .iter()
                .map(|field| {
                    rate(
                        prev.vmstat.get(*field).copied(),
                        cur.vmstat.get(*field).copied(),
                        interval_s,
                    )
                })
                .collect(),
        },
        disks,
    })
}

/// A counter's increase per second; None when either reading is missing or
/// the counter went backwards.
fn rate(prev: Option<u64>, cur: Option<u64>, interval_s: f64) -> Option<f64> {
    Some(cur?.checked_sub(prev?)? as f64 / interval_s)
}

/// A CPU counter that steps back counts as no time, as in sysstat: `iowait`
/// does so on Linux, and `user` minus `guest` can when the two are not
/// updated together. The shares are taken over the sum of what remains, so
/// they add up to 100 whatever the length of the interval.
fn cpu_shares(prev: &CpuTimes, cur: &CpuTimes) -> Option<CpuShares> {
    let diff = |before: u64, after: u64| after.saturating_sub(before) as f64;
    let user = diff(
        prev.user.saturating_sub(prev.guest),
        cur.user.saturating_sub(cur.guest),
    );
    let nice = diff(
        prev.nice.saturating_sub(prev.guest_nice),
        cur.nice.saturating_sub(cur.guest_nice),
    );
    let system = diff(prev.system, cur.system);
    let idle = diff(prev.idle, cur.idle);
    let iowait = diff(prev.iowait, cur.iowait);
    let irq = diff(prev.irq, cur.irq);
    let softirq = diff(prev.softirq, cur.softirq);
    let steal = diff(prev.steal, cur.steal);
    let guest = diff(prev.guest, cur.guest);
    let guest_nice = diff(prev.guest_nice, cur.guest_nice);

    let total = user + nice + system + idle + iowait + irq + softirq + steal + guest + guest_nice;
    if total <= 0.0 {
        return None;
    }
    let share = |ticks: f64| ticks * 100.0 / total;
    Some(CpuShares {
        user: share(user),
        nice: share(nice),
        system: share(system),
        iowait: share(iowait),
        irq: share(irq),
        softirq: share(softirq),
        steal: share(steal),
        guest: share(guest),
        guest_nice: share(guest_nice),
        idle: share(idle),
    })
}

fn pressure_shares(
    prev: Option<Pressure>,
    cur: Option<Pressure>,
    interval_s: f64,
) -> PressureShares {
    // Microseconds stalled per second of interval, as a percentage.
    let share = |before: Option<u64>, after: Option<u64>| {
        rate(before, after, interval_s).map(|us_per_s| (us_per_s / 10_000.0).min(100.0))
    };
    PressureShares {
        some: share(prev.map(|p| p.some_us), cur.map(|p| p.some_us)),
        full: share(prev.and_then(|p| p.full_us), cur.and_then(|p| p.full_us)),
    }
}

/// A loop, RAM or compressed-RAM device: not storage worth a series.
pub fn is_pseudo(name: &str) -> bool {
    ["loop", "ram", "zram"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Whole devices that have done I/O since boot. Partitions are left out
/// (their I/O is their device's), as are the loop, RAM and compressed-RAM
/// devices. A host that does not list its block devices has every line kept.
fn is_monitored(name: &str, counters: &DiskCounters, blockdevs: &[String]) -> bool {
    !is_pseudo(name)
        && (blockdevs.is_empty() || blockdevs.iter().any(|device| device == name))
        && counters.reads + counters.writes > 0
}

/// None when one of the device's counters went backwards: the device was
/// removed and added again between the two samples.
fn disk_sample(
    device: &str,
    prev: &DiskCounters,
    cur: &DiskCounters,
    interval_s: f64,
) -> Option<DiskSample> {
    let reads = cur.reads.checked_sub(prev.reads)? as f64;
    let writes = cur.writes.checked_sub(prev.writes)? as f64;
    let rd_kb = cur.rd_sectors.checked_sub(prev.rd_sectors)? as f64 / 2.0;
    let wr_kb = cur.wr_sectors.checked_sub(prev.wr_sectors)? as f64 / 2.0;
    let rd_ms = cur.rd_ms.checked_sub(prev.rd_ms)? as f64;
    let wr_ms = cur.wr_ms.checked_sub(prev.wr_ms)? as f64;
    let io_ms = cur.io_ms.checked_sub(prev.io_ms)? as f64;
    let weighted_ms = cur.weighted_ms.checked_sub(prev.weighted_ms)? as f64;

    let per = |sum: f64, count: f64| if count > 0.0 { sum / count } else { 0.0 };
    let interval_ms = interval_s * 1000.0;
    let discards = |pick: fn((u64, u64)) -> u64| {
        rate(prev.discards.map(pick), cur.discards.map(pick), interval_s)
    };

    let flushes = |pick: fn((u64, u64)) -> u64| {
        rate(prev.flushes.map(pick), cur.flushes.map(pick), interval_s)
    };

    Some(DiskSample {
        device: device.to_string(),
        r_s: reads / interval_s,
        w_s: writes / interval_s,
        rkb_s: rd_kb / interval_s,
        wkb_s: wr_kb / interval_s,
        r_await_ms: per(rd_ms, reads),
        w_await_ms: per(wr_ms, writes),
        rareq_kb: per(rd_kb, reads),
        wareq_kb: per(wr_kb, writes),
        aqu_sz: weighted_ms / interval_ms,
        // The interval has the resolution of /proc/uptime (10 ms), so a
        // device busy all along can come out a little above 100.
        util_pct: (io_ms * 100.0 / interval_ms).min(100.0),
        d_s: discards(|(count, _)| count),
        dkb_s: discards(|(_, sectors)| sectors).map(|sectors_s| sectors_s / 2.0),
        f_s: flushes(|(count, _)| count),
        f_await_ms: flushes(|(count, _)| count)
            .zip(flushes(|(_, ms)| ms))
            .map(|(count_s, ms_s)| per(ms_s, count_s)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::sysmon::parse::{LoadAvg, Stat};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn parsed(uptime_s: f64, cpu: CpuTimes) -> Parsed {
        Parsed {
            uptime_s,
            stat: Stat {
                cpu,
                cpus: 2,
                ..Stat::default()
            },
            ..Parsed::default()
        }
    }

    fn disk(reads: u64, writes: u64) -> DiskCounters {
        DiskCounters {
            reads,
            writes,
            ..DiskCounters::default()
        }
    }

    #[test]
    fn cpu_shares_add_up_to_100_and_take_guest_time_out_of_user() {
        let before = CpuTimes {
            user: 1000,
            nice: 100,
            system: 500,
            idle: 8000,
            iowait: 200,
            irq: 10,
            softirq: 20,
            steal: 5,
            guest: 300,
            guest_nice: 40,
        };
        // 200 ticks over the interval: user +80 of which guest +20,
        // nice +10 of which guest_nice +10, system +30, idle +60, iowait +10,
        // irq +2, softirq +4, steal +4.
        let after = CpuTimes {
            user: 1080,
            nice: 110,
            system: 530,
            idle: 8060,
            iowait: 210,
            irq: 12,
            softirq: 24,
            steal: 9,
            guest: 320,
            guest_nice: 50,
        };
        let cpu = cpu_shares(&before, &after).unwrap();
        assert!(close(cpu.user, 30.0), "{cpu:?}");
        assert!(close(cpu.guest, 10.0));
        assert!(close(cpu.nice, 0.0));
        assert!(close(cpu.guest_nice, 5.0));
        assert!(close(cpu.system, 15.0));
        assert!(close(cpu.idle, 30.0));
        assert!(close(cpu.iowait, 5.0));
        assert!(close(cpu.irq, 1.0));
        assert!(close(cpu.softirq, 2.0));
        assert!(close(cpu.steal, 2.0));
        assert!(close(cpu.total(), 100.0));
        assert!(close(cpu.busy(), 65.0));
    }

    #[test]
    fn a_cpu_counter_stepping_back_counts_as_no_time() {
        let before = CpuTimes {
            user: 100,
            idle: 900,
            iowait: 50,
            ..CpuTimes::default()
        };
        let after = CpuTimes {
            user: 150,
            idle: 950,
            iowait: 48,
            ..CpuTimes::default()
        };
        let cpu = cpu_shares(&before, &after).unwrap();
        assert!(close(cpu.iowait, 0.0));
        assert!(close(cpu.user, 50.0));
        assert!(close(cpu.idle, 50.0));
    }

    #[test]
    fn no_cpu_time_gives_no_shares() {
        let times = CpuTimes {
            user: 100,
            idle: 900,
            ..CpuTimes::default()
        };
        assert_eq!(cpu_shares(&times, &times), None);
    }

    #[test]
    fn a_clock_that_did_not_advance_gives_no_sample() {
        let a = parsed(100.0, CpuTimes::default());
        assert_eq!(delta(&a, &a, &[]), None);
        let earlier = parsed(99.0, CpuTimes::default());
        assert_eq!(delta(&a, &earlier, &[]), None);
    }

    #[test]
    fn rates_divide_by_the_hosts_own_interval() {
        let mut before = parsed(100.0, CpuTimes::default());
        let mut after = parsed(102.5, CpuTimes::default());
        before.stat.ctxt = Some(10_000);
        after.stat.ctxt = Some(15_000);
        before.stat.processes = Some(700);
        after.stat.processes = Some(650); // went backwards
        after.stat.procs_running = Some(4);
        after.stat.procs_blocked = Some(1);
        after.loadavg = Some(LoadAvg {
            load1: 1.5,
            load5: 1.0,
            load15: 0.5,
        });
        before.vmstat.insert("pgpgin".into(), 1_000);
        after.vmstat.insert("pgpgin".into(), 1_250);
        after.vmstat.insert("pswpin".into(), 9); // no earlier reading
        after.meminfo.insert("MemTotal".into(), 16_314_388);
        after.meminfo.insert("Shmem".into(), 4_194_304);

        let sample = delta(&before, &after, &[]).unwrap();
        assert!(close(sample.interval_s, 2.5));
        assert_eq!(sample.activity.ctxt_s, Some(2000.0));
        assert_eq!(sample.activity.forks_s, None);
        assert_eq!(sample.activity.procs_running, Some(4));
        assert_eq!(sample.activity.procs_blocked, Some(1));
        assert_eq!(sample.activity.load1, Some(1.5));
        assert_eq!(sample.memory.rate("pgpgin"), Some(100.0));
        assert_eq!(sample.memory.rate("pswpin"), None);
        assert_eq!(sample.memory.rate("pgmajfault"), None);
        assert_eq!(sample.memory.gauge("MemTotal"), Some(16_314_388));
        assert_eq!(sample.memory.gauge("Shmem"), Some(4_194_304));
        assert_eq!(sample.memory.gauge("HugePages_Total"), None);
        assert_eq!(sample.memory.gauges.len(), MEM_GAUGES.len());
        assert_eq!(sample.memory.rates.len(), VM_RATES.len());
    }

    #[test]
    fn pressure_is_the_stalled_share_of_the_interval() {
        let mut before = parsed(10.0, CpuTimes::default());
        let mut after = parsed(12.0, CpuTimes::default());
        before.pressure_io = Some(Pressure {
            some_us: 1_000_000,
            full_us: Some(500_000),
        });
        after.pressure_io = Some(Pressure {
            some_us: 1_500_000, // 0.5 s stalled in 2 s
            full_us: Some(600_000),
        });
        before.pressure_cpu = Some(Pressure {
            some_us: 0,
            full_us: None,
        });
        after.pressure_cpu = Some(Pressure {
            some_us: 2_000_000,
            full_us: None,
        });

        let sample = delta(&before, &after, &[]).unwrap();
        assert_eq!(sample.pressure_io.some, Some(25.0));
        assert_eq!(sample.pressure_io.full, Some(5.0));
        assert_eq!(sample.pressure_cpu.some, Some(100.0));
        assert_eq!(sample.pressure_cpu.full, None);
        assert_eq!(sample.pressure_memory, PressureShares::default());
    }

    #[test]
    fn disk_figures_match_iostat() {
        let before = DiskCounters {
            reads: 1000,
            rd_sectors: 80_000,
            rd_ms: 500,
            writes: 2000,
            wr_sectors: 160_000,
            wr_ms: 4000,
            in_flight: 0,
            io_ms: 3000,
            weighted_ms: 4500,
            discards: Some((10, 8192)),
            flushes: Some((100, 300)),
        };
        // Over 2 s: 200 reads of 8 kB taking 0.5 ms each, 400 writes of
        // 16 kB taking 2 ms each, 1 s busy, 0.9 s of requests in flight.
        let after = DiskCounters {
            reads: 1200,
            rd_sectors: 83_200,
            rd_ms: 600,
            writes: 2400,
            wr_sectors: 172_800,
            wr_ms: 4800,
            in_flight: 3,
            io_ms: 4000,
            weighted_ms: 5400,
            discards: Some((14, 16_384)),
            flushes: Some((120, 330)),
        };
        let disk = disk_sample("nvme0n1", &before, &after, 2.0).unwrap();
        assert_eq!(disk.device, "nvme0n1");
        assert!(close(disk.r_s, 100.0), "{disk:?}");
        assert!(close(disk.w_s, 200.0));
        assert!(close(disk.rkb_s, 800.0));
        assert!(close(disk.wkb_s, 3200.0));
        assert!(close(disk.r_await_ms, 0.5));
        assert!(close(disk.w_await_ms, 2.0));
        assert!(close(disk.rareq_kb, 8.0));
        assert!(close(disk.wareq_kb, 16.0));
        assert!(close(disk.aqu_sz, 0.45));
        assert!(close(disk.util_pct, 50.0));
        assert_eq!(disk.d_s, Some(2.0));
        assert_eq!(disk.dkb_s, Some(2048.0));
        assert_eq!(disk.f_s, Some(10.0));
        // 20 flushes taking 30 ms together.
        assert!(close(disk.f_await_ms.unwrap(), 1.5));
    }

    #[test]
    fn an_idle_disk_has_zero_awaits_and_no_optional_figures() {
        let counters = DiskCounters {
            reads: 10,
            writes: 10,
            ..DiskCounters::default()
        };
        let disk = disk_sample("sda", &counters, &counters, 1.0).unwrap();
        assert_eq!(disk.r_await_ms, 0.0);
        assert_eq!(disk.wareq_kb, 0.0);
        assert_eq!(disk.util_pct, 0.0);
        assert_eq!((disk.d_s, disk.dkb_s, disk.f_s), (None, None, None));
        assert_eq!(disk.f_await_ms, None);
    }

    #[test]
    fn utilization_is_capped_at_100() {
        let before = disk(10, 10);
        let after = DiskCounters {
            io_ms: 1010,
            ..disk(20, 20)
        };
        let sample = disk_sample("sda", &before, &after, 1.0).unwrap();
        assert_eq!(sample.util_pct, 100.0);
    }

    #[test]
    fn only_whole_devices_that_did_io_are_monitored() {
        let blockdevs: Vec<String> = ["nvme0n1", "sda", "dm-0", "loop0", "zram0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let active = disk(5, 5);
        assert!(is_monitored("nvme0n1", &active, &blockdevs));
        assert!(is_monitored("dm-0", &active, &blockdevs));
        assert!(!is_monitored("nvme0n1p1", &active, &blockdevs));
        assert!(!is_monitored("loop0", &active, &blockdevs));
        assert!(!is_monitored("zram0", &active, &blockdevs));
        assert!(!is_monitored("ram12", &active, &[]));
        assert!(!is_monitored("sda", &disk(0, 0), &blockdevs));
        // Without the host's list, every real line is kept.
        assert!(is_monitored("sda1", &active, &[]));
    }

    #[test]
    fn a_device_that_came_back_or_just_appeared_is_skipped_once() {
        let mut before = parsed(10.0, CpuTimes::default());
        let mut after = parsed(11.0, CpuTimes::default());
        before.disks.insert("sda".into(), disk(100, 100));
        before.disks.insert("sdb".into(), disk(500, 500));
        after.disks.insert("sda".into(), disk(150, 100));
        after.disks.insert("sdb".into(), disk(3, 1)); // counters restarted
        after.disks.insert("sdc".into(), disk(7, 7)); // not there before

        let sample = delta(&before, &after, &[]).unwrap();
        let devices: Vec<&str> = sample.disks.iter().map(|d| d.device.as_str()).collect();
        assert_eq!(devices, ["sda"]);
        assert!(close(sample.disks[0].r_s, 50.0));
    }
}
