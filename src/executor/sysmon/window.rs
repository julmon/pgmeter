//! A host's figures over the measured window (the samples taken after the
//! ramp-up), for the results and the report: what the cards and the volume
//! table show.

use super::delta::DiskSample;
use super::devices::Device;
use super::{HostSeries, TimedSample};

/// A volume that never reached this share of time busy during the run, and
/// on which PostgreSQL keeps nothing, is left out: a swap volume touched a
/// few times.
pub const NEAR_IDLE_PCT: f64 = 1.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AvgMax {
    pub avg: f64,
    pub max: f64,
}

impl AvgMax {
    fn of(values: impl Iterator<Item = f64>) -> Option<AvgMax> {
        let values: Vec<f64> = values.collect();
        if values.is_empty() {
            return None;
        }
        Some(AvgMax {
            avg: values.iter().sum::<f64>() / values.len() as f64,
            max: values.iter().copied().fold(f64::MIN, f64::max),
        })
    }

    fn json(value: Option<AvgMax>) -> serde_json::Value {
        match value {
            Some(v) => serde_json::json!({"avg": v.avg, "max": v.max}),
            None => serde_json::Value::Null,
        }
    }
}

/// The top of a device stack (what a file system sits on) over the window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VolumeSummary {
    /// The kernel name, as in the disk CSV file.
    pub device: String,
    /// The mapper name when there is one, else the kernel name.
    pub name: String,
    /// What PostgreSQL keeps on it; empty when unknown or nothing.
    pub holds: Vec<String>,
    /// The layers under it, top first: `["nvme0n1p3_crypt", "nvme0n1"]`; a
    /// layer of several devices reads `sda + sdb`.
    pub on: Vec<String>,
    pub written_mb_s: AvgMax,
    pub read_mb_s: AvgMax,
    /// Reads and writes completed per second.
    pub requests_s: AvgMax,
    /// Average time of a write, and of a read, over all those of the window.
    pub write_ms: f64,
    pub read_ms: f64,
    /// The disk under the volume whose flushes are shown, and their average
    /// time: the kernel counts flushes at the disk, not on the volume.
    pub flush_device: Option<String>,
    pub flush_ms: Option<AvgMax>,
    pub busy_pct: AvgMax,
}

impl VolumeSummary {
    /// `vg-data (data)`, or the bare name when nothing is known of it.
    pub fn titled(&self) -> String {
        if self.holds.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.holds.join(" + "))
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "device": self.device,
            "name": self.name,
            "holds": self.holds,
            "on": self.on,
            "written_mb_s": AvgMax::json(Some(self.written_mb_s)),
            "read_mb_s": AvgMax::json(Some(self.read_mb_s)),
            "requests_s": AvgMax::json(Some(self.requests_s)),
            "write_ms": self.write_ms,
            "read_ms": self.read_ms,
            "flush_device": self.flush_device,
            "flush_ms": AvgMax::json(self.flush_ms),
            "busy_pct": AvgMax::json(Some(self.busy_pct)),
        })
    }
}

/// A host over the measured window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostSummary {
    pub target: String,
    pub host: String,
    pub database: bool,
    /// Samples of the window; the figures below are None or empty at zero.
    pub samples: usize,
    /// Share of CPU time not idle.
    pub cpu_busy_pct: Option<AvgMax>,
    pub cpu_user_pct: Option<AvgMax>,
    pub cpu_system_pct: Option<AvgMax>,
    pub cpu_iowait_pct: Option<AvgMax>,
    pub cpu_steal_pct: Option<AvgMax>,
    /// `MemTotal - MemAvailable`, in kB.
    pub mem_used_kb: Option<AvgMax>,
    pub mem_total_kb: u64,
    /// Pages swapped in and out per second, together.
    pub swap_pages_s: Option<AvgMax>,
    /// Share of time some task waited for I/O (pressure stall information).
    pub io_pressure_pct: Option<AvgMax>,
    /// The volumes drawn, PostgreSQL's data first, then its WAL.
    pub volumes: Vec<VolumeSummary>,
    /// Volumes left out as near idle, by name.
    pub idle_volumes: Vec<String>,
    /// kB written per second to all the volumes together.
    pub written_mb_s: Option<AvgMax>,
}

impl HostSummary {
    /// The volume with the highest average share of time busy.
    #[cfg(test)]
    pub fn busiest(&self) -> Option<&VolumeSummary> {
        self.volumes
            .iter()
            .max_by(|a, b| a.busy_pct.avg.total_cmp(&b.busy_pct.avg))
    }

    /// The volume PostgreSQL keeps its WAL on, when known.
    #[cfg(test)]
    pub fn wal(&self) -> Option<&VolumeSummary> {
        self.volumes
            .iter()
            .find(|volume| volume.holds.iter().any(|what| what == "WAL"))
    }

    /// The host's entry in the `system` array of `--format json`.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "target": self.target,
            "host": self.host,
            "database": self.database,
            "samples": self.samples,
            "cpu": {
                "busy_pct": AvgMax::json(self.cpu_busy_pct),
                "user_pct": AvgMax::json(self.cpu_user_pct),
                "system_pct": AvgMax::json(self.cpu_system_pct),
                "iowait_pct": AvgMax::json(self.cpu_iowait_pct),
                "steal_pct": AvgMax::json(self.cpu_steal_pct),
            },
            "memory": {
                "total_kb": self.mem_total_kb,
                "used_kb": AvgMax::json(self.mem_used_kb),
                "swap_pages_s": AvgMax::json(self.swap_pages_s),
            },
            "io_pressure_pct": AvgMax::json(self.io_pressure_pct),
            "written_mb_s": AvgMax::json(self.written_mb_s),
            "volumes": self.volumes.iter().map(VolumeSummary::json).collect::<Vec<_>>(),
            "idle_volumes": self.idle_volumes,
        })
    }
}

/// The physical devices at the bottom of a device's stack; the device
/// itself when nothing is under it.
fn leaves(name: &str, devices: &[Device]) -> Vec<String> {
    match devices.iter().find(|d| d.name == name) {
        Some(device) if !device.on.is_empty() => {
            let mut found: Vec<String> = Vec::new();
            for below in &device.on {
                for leaf in leaves(below, devices) {
                    if !found.contains(&leaf) {
                        found.push(leaf);
                    }
                }
            }
            found
        }
        _ => vec![name.to_string()],
    }
}

/// The layers under a device, top first, by display name.
fn layers(name: &str, devices: &[Device]) -> Vec<String> {
    let display = |name: &String| {
        devices
            .iter()
            .find(|d| d.name == *name)
            .and_then(|d| d.mapper_name.clone())
            .unwrap_or_else(|| name.clone())
    };
    let mut found = Vec::new();
    let mut current = name.to_string();
    while let Some(device) = devices.iter().find(|d| d.name == current) {
        if device.on.is_empty() {
            break;
        }
        found.push(
            device
                .on
                .iter()
                .map(display)
                .collect::<Vec<_>>()
                .join(" + "),
        );
        current = device.on[0].clone();
    }
    found
}

fn disk<'a>(sample: &'a TimedSample, device: &str) -> Option<&'a DiskSample> {
    sample.sample.disks.iter().find(|d| d.device == device)
}

/// `sum(time * count) / sum(count)`: the average over the requests, not
/// over the samples.
fn per_request(pairs: impl Iterator<Item = (f64, f64)>) -> f64 {
    let (time, count) = pairs.fold((0.0, 0.0), |(time, count), (ms, rate)| {
        (time + ms * rate, count + rate)
    });
    if count > 0.0 {
        time / count
    } else {
        0.0
    }
}

/// The figures of `series` over the samples whose interval began at or
/// after `window_start` (Unix seconds); all of them when it is None.
pub fn summarize(series: &HostSeries, window_start: Option<i64>) -> HostSummary {
    let window: Vec<&TimedSample> = series
        .samples
        .iter()
        .filter(|s| {
            window_start.is_none_or(|start| s.timestamp - s.sample.interval_s >= start as f64)
        })
        .collect();
    let over = |pick: &dyn Fn(&TimedSample) -> Option<f64>| {
        AvgMax::of(window.iter().filter_map(|s| pick(s)))
    };
    let cpu = |pick: fn(&super::delta::CpuShares) -> f64| {
        over(&|s: &TimedSample| s.sample.cpu.as_ref().map(pick))
    };

    let info = &series.info;
    // A device is drawn when it is the top of its stack, did some I/O, and
    // was not near idle from one end of the run to the other.
    let mut volumes: Vec<VolumeSummary> = Vec::new();
    let mut idle_volumes = Vec::new();
    for device in info.devices.iter().filter(|d| d.top) {
        let name = device
            .mapper_name
            .clone()
            .unwrap_or_else(|| device.name.clone());
        let all = || series.samples.iter().filter_map(|s| disk(s, &device.name));
        if all().next().is_none() {
            continue;
        }
        if device.holds.is_empty() && !all().any(|d| d.util_pct >= NEAR_IDLE_PCT) {
            idle_volumes.push(name);
            continue;
        }
        let rows = || window.iter().filter_map(|s| disk(s, &device.name));
        let stat = |pick: fn(&DiskSample) -> f64| AvgMax::of(rows().map(pick)).unwrap_or_default();

        // The disk under the volume that flushed most: one flush goes to
        // every member of a RAID array, so they are not summed.
        let flush_rate = |leaf: &str| {
            AvgMax::of(
                window
                    .iter()
                    .filter_map(|s| disk(s, leaf).and_then(|d| d.f_s)),
            )
            .map_or(0.0, |v| v.avg)
        };
        let flush_device = leaves(&device.name, &info.devices)
            .into_iter()
            .filter(|leaf| flush_rate(leaf) > 0.0)
            .max_by(|a, b| flush_rate(a).total_cmp(&flush_rate(b)));
        let flush_ms = flush_device.as_ref().and_then(|leaf| {
            AvgMax::of(
                window
                    .iter()
                    .filter_map(|s| disk(s, leaf).and_then(|d| d.f_await_ms)),
            )
        });

        volumes.push(VolumeSummary {
            device: device.name.clone(),
            name,
            holds: device.holds.clone(),
            on: layers(&device.name, &info.devices),
            written_mb_s: stat(|d| d.wkb_s / 1024.0),
            read_mb_s: stat(|d| d.rkb_s / 1024.0),
            requests_s: stat(|d| d.r_s + d.w_s),
            write_ms: per_request(rows().map(|d| (d.w_await_ms, d.w_s))),
            read_ms: per_request(rows().map(|d| (d.r_await_ms, d.r_s))),
            flush_device,
            flush_ms,
            busy_pct: stat(|d| d.util_pct),
        });
    }
    // PostgreSQL's data first, then its WAL, then the rest by name.
    let rank = |volume: &VolumeSummary| {
        if volume.holds.iter().any(|what| what == "data") {
            0
        } else if volume.holds.iter().any(|what| what == "WAL") {
            1
        } else if !volume.holds.is_empty() {
            2
        } else {
            3
        }
    };
    volumes.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));

    let written_mb_s = if volumes.is_empty() {
        None
    } else {
        AvgMax::of(window.iter().map(|s| {
            volumes
                .iter()
                .filter_map(|volume| disk(s, &volume.device))
                .map(|d| d.wkb_s / 1024.0)
                .sum()
        }))
    };

    HostSummary {
        target: info.target.clone(),
        host: info.host.clone(),
        database: info.database,
        samples: window.len(),
        cpu_busy_pct: cpu(|c| c.busy()),
        cpu_user_pct: cpu(|c| c.user),
        cpu_system_pct: cpu(|c| c.system),
        cpu_iowait_pct: cpu(|c| c.iowait),
        cpu_steal_pct: cpu(|c| c.steal),
        mem_used_kb: over(&|s: &TimedSample| {
            let total = s.sample.memory.gauge("MemTotal")?;
            let available = s.sample.memory.gauge("MemAvailable")?;
            Some(total.saturating_sub(available) as f64)
        }),
        mem_total_kb: info.mem_total_kb,
        swap_pages_s: over(&|s: &TimedSample| {
            Some(s.sample.memory.rate("pswpin")? + s.sample.memory.rate("pswpout")?)
        }),
        io_pressure_pct: over(&|s: &TimedSample| s.sample.pressure_io.some),
        volumes,
        idle_volumes,
        written_mb_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::sysmon::delta::{CpuShares, MemorySample, Sample, MEM_GAUGES, VM_RATES};
    use crate::executor::sysmon::HostInfo;

    fn device(name: &str, mapper: Option<&str>, on: &[&str], top: bool, holds: &[&str]) -> Device {
        Device {
            name: name.to_string(),
            mapper_name: mapper.map(str::to_string),
            on: on.iter().map(|s| s.to_string()).collect(),
            top,
            holds: holds.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn disk_sample(device: &str, wkb_s: f64, w_s: f64, w_await_ms: f64, util: f64) -> DiskSample {
        DiskSample {
            device: device.to_string(),
            w_s,
            wkb_s,
            w_await_ms,
            util_pct: util,
            f_s: Some(0.0),
            f_await_ms: Some(0.0),
            ..DiskSample::default()
        }
    }

    fn sample(timestamp: f64, idle: f64, available_kb: u64, disks: Vec<DiskSample>) -> TimedSample {
        let mut gauges = vec![None; MEM_GAUGES.len()];
        gauges[0] = Some(16_000_000);
        gauges[2] = Some(available_kb);
        TimedSample {
            timestamp,
            sample: Sample {
                interval_s: 1.0,
                cpu: Some(CpuShares {
                    user: 100.0 - idle,
                    idle,
                    ..CpuShares::default()
                }),
                memory: MemorySample {
                    gauges,
                    rates: vec![Some(0.0); VM_RATES.len()],
                },
                disks,
                ..Sample::default()
            },
        }
    }

    fn series(devices: Vec<Device>, samples: Vec<TimedSample>) -> HostSeries {
        HostSeries {
            info: HostInfo {
                target: "postgres@db1".into(),
                label: "db1".into(),
                host: "db1".into(),
                kernel: "6.1".into(),
                cpus: 8,
                mem_total_kb: 16_000_000,
                interval_s: 1.0,
                database: true,
                devices,
            },
            samples,
            truncated: None,
        }
    }

    // Data on a logical volume over a RAID 1, WAL on its own disk, and a
    // swap volume that is touched once.
    fn host() -> HostSeries {
        let devices = vec![
            device("dm-0", Some("vg-data"), &["md0"], true, &["data"]),
            device("dm-1", Some("vg-swap"), &["md0"], true, &[]),
            device("md0", None, &["sda", "sdb"], false, &[]),
            device("nvme0n1", None, &[], true, &["WAL"]),
            device("sda", None, &[], false, &[]),
            device("sdb", None, &[], false, &[]),
        ];
        let disks = |data_kb: f64, wal_ms: f64, flush_ms: f64| {
            let mut sda = disk_sample("sda", data_kb, 100.0, 2.0, 40.0);
            sda.f_s = Some(10.0);
            sda.f_await_ms = Some(flush_ms);
            let mut sdb = disk_sample("sdb", data_kb, 100.0, 2.0, 40.0);
            sdb.f_s = Some(12.0);
            sdb.f_await_ms = Some(flush_ms + 1.0);
            let mut wal = disk_sample("nvme0n1", 1024.0, 500.0, wal_ms, 90.0);
            wal.f_s = Some(400.0);
            wal.f_await_ms = Some(1.5);
            vec![
                disk_sample("dm-0", data_kb, 100.0, 2.0, 40.0),
                disk_sample("dm-1", 4.0, 1.0, 1.0, 0.2),
                disk_sample("md0", data_kb, 100.0, 2.0, 40.0),
                wal,
                sda,
                sdb,
            ]
        };
        series(
            devices,
            vec![
                // The ramp-up: left out of every figure.
                sample(100.5, 90.0, 9_000_000, disks(99_999.0, 9.0, 9.0)),
                sample(101.5, 40.0, 8_000_000, disks(2048.0, 1.0, 4.0)),
                sample(102.5, 20.0, 6_000_000, disks(4096.0, 3.0, 6.0)),
            ],
        )
    }

    #[test]
    fn the_figures_cover_the_measured_window_only() {
        let summary = summarize(&host(), Some(100));
        // The first sample began at 99.5, inside the ramp-up.
        assert_eq!(summary.samples, 2);
        let busy = summary.cpu_busy_pct.unwrap();
        assert_eq!((busy.avg, busy.max), (70.0, 80.0));
        let used = summary.mem_used_kb.unwrap();
        assert_eq!((used.avg, used.max), (9_000_000.0, 10_000_000.0));
        // Without a ramp-up every sample counts.
        assert_eq!(summarize(&host(), None).samples, 3);
    }

    #[test]
    fn volumes_are_the_tops_of_the_stacks_with_the_data_first() {
        let summary = summarize(&host(), Some(100));
        let names: Vec<String> = summary.volumes.iter().map(VolumeSummary::titled).collect();
        assert_eq!(names, ["vg-data (data)", "nvme0n1 (WAL)"]);
        // The swap volume never reached 1% busy.
        assert_eq!(summary.idle_volumes, ["vg-swap"]);

        let data = &summary.volumes[0];
        assert_eq!(data.on, ["md0", "sda + sdb"]);
        assert_eq!((data.written_mb_s.avg, data.written_mb_s.max), (3.0, 4.0));
        // Its flushes are those of the member that flushed most.
        assert_eq!(data.flush_device.as_deref(), Some("sdb"));
        assert_eq!(data.flush_ms.unwrap().avg, 6.0);

        let wal = summary.wal().unwrap();
        assert_eq!(wal.flush_device.as_deref(), Some("nvme0n1"));
        assert_eq!(wal.write_ms, 2.0);
        assert_eq!(summary.busiest().unwrap().device, "nvme0n1");
        // Written: the two volumes together, 3 + 1 MB/s on average.
        assert_eq!(summary.written_mb_s.unwrap().avg, 4.0);
    }

    #[test]
    fn request_times_average_over_the_requests() {
        // 900 writes at 1 ms and 100 at 11 ms: 2 ms per write, not 6.
        let devices = vec![device("sda", None, &[], true, &[])];
        let samples = vec![
            sample(
                1.0,
                50.0,
                1,
                vec![disk_sample("sda", 1.0, 900.0, 1.0, 50.0)],
            ),
            sample(
                2.0,
                50.0,
                1,
                vec![disk_sample("sda", 1.0, 100.0, 11.0, 50.0)],
            ),
        ];
        let summary = summarize(&series(devices, samples), None);
        assert_eq!(summary.volumes[0].write_ms, 2.0);
        assert_eq!(summary.volumes[0].read_ms, 0.0);
        assert_eq!(summary.volumes[0].flush_device, None);
    }

    #[test]
    fn a_host_without_a_sample_in_the_window_has_no_figure() {
        let summary = summarize(&host(), Some(1000));
        assert_eq!(summary.samples, 0);
        assert_eq!(summary.cpu_busy_pct, None);
        assert_eq!(summary.json()["cpu"]["busy_pct"], serde_json::Value::Null);
        // The volumes are still named: they did I/O during the run.
        assert_eq!(summary.volumes.len(), 2);
    }
}
