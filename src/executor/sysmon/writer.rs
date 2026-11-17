//! The per-host CSV files of the results directory: one row per sample for
//! CPU and memory, one row per sample and device for the disks.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::delta::{PressureShares, Sample, MEM_GAUGES, VM_RATES};

const CPU_HEADER: &str = "timestamp,user_pct,nice_pct,system_pct,iowait_pct,irq_pct,softirq_pct,\
steal_pct,guest_pct,guest_nice_pct,idle_pct,ctxt_s,forks_s,procs_running,procs_blocked,\
load1,load5,load15,psi_cpu_some_pct,psi_cpu_full_pct,psi_memory_some_pct,psi_memory_full_pct,\
psi_io_some_pct,psi_io_full_pct";

const DISK_HEADER: &str = "timestamp,device,r_s,w_s,rkb_s,wkb_s,r_await_ms,w_await_ms,\
rareq_kb,wareq_kb,aqu_sz,util_pct,d_s,dkb_s,f_s,f_await_ms";

/// The names of a host's three files, in the order cpu, memory, disk.
pub fn file_names(label: &str) -> [String; 3] {
    ["cpu", "memory", "disk"].map(|kind| format!("pgmeter-system-{label}-{kind}.csv"))
}

pub struct Writer {
    cpu: BufWriter<File>,
    memory: BufWriter<File>,
    disk: BufWriter<File>,
}

impl Writer {
    /// Creates the three files of the host named `label` with their headers.
    pub fn create(dir: &Path, label: &str) -> std::io::Result<Writer> {
        let [cpu, memory, disk] = file_names(label);
        let open = |name: String| File::create(dir.join(name)).map(BufWriter::new);
        let mut writer = Writer {
            cpu: open(cpu)?,
            memory: open(memory)?,
            disk: open(disk)?,
        };
        writeln!(writer.cpu, "{CPU_HEADER}")?;
        writeln!(writer.memory, "{}", memory_header())?;
        writeln!(writer.disk, "{DISK_HEADER}")?;
        writer.flush()?;
        Ok(writer)
    }

    /// Appends one sample, taken at `timestamp` (Unix seconds), and flushes,
    /// so the files can be followed while the run goes.
    pub fn write(&mut self, timestamp: f64, sample: &Sample) -> std::io::Result<()> {
        writeln!(self.cpu, "{}", cpu_row(timestamp, sample))?;
        writeln!(self.memory, "{}", memory_row(timestamp, sample))?;
        for row in disk_rows(timestamp, sample) {
            writeln!(self.disk, "{row}")?;
        }
        self.flush()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.cpu.flush()?;
        self.memory.flush()?;
        self.disk.flush()
    }
}

/// `timestamp`, the `/proc/meminfo` fields under their kernel names, then
/// the `/proc/vmstat` rates as `<counter>_s`.
fn memory_header() -> String {
    let mut columns = vec!["timestamp".to_string()];
    columns.extend(MEM_GAUGES.iter().map(|field| field.to_string()));
    columns.extend(VM_RATES.iter().map(|field| format!("{field}_s")));
    columns.join(",")
}

/// A figure the host does not report is an empty cell, never a zero.
fn float(value: Option<f64>, decimals: usize) -> String {
    value.map_or_else(String::new, |v| format!("{v:.decimals$}"))
}

fn integer(value: Option<u64>) -> String {
    value.map_or_else(String::new, |v| v.to_string())
}

fn cpu_row(timestamp: f64, sample: &Sample) -> String {
    let mut cells = vec![format!("{timestamp:.3}")];
    let shares = sample.cpu.map(|c| {
        [
            c.user,
            c.nice,
            c.system,
            c.iowait,
            c.irq,
            c.softirq,
            c.steal,
            c.guest,
            c.guest_nice,
            c.idle,
        ]
    });
    for i in 0..10 {
        cells.push(float(shares.map(|s| s[i]), 2));
    }
    let activity = &sample.activity;
    cells.push(float(activity.ctxt_s, 1));
    cells.push(float(activity.forks_s, 1));
    cells.push(integer(activity.procs_running));
    cells.push(integer(activity.procs_blocked));
    cells.push(float(activity.load1, 2));
    cells.push(float(activity.load5, 2));
    cells.push(float(activity.load15, 2));
    let pressures: [&PressureShares; 3] = [
        &sample.pressure_cpu,
        &sample.pressure_memory,
        &sample.pressure_io,
    ];
    for pressure in pressures {
        cells.push(float(pressure.some, 2));
        cells.push(float(pressure.full, 2));
    }
    cells.join(",")
}

fn memory_row(timestamp: f64, sample: &Sample) -> String {
    let mut cells = vec![format!("{timestamp:.3}")];
    cells.extend(sample.memory.gauges.iter().map(|gauge| integer(*gauge)));
    cells.extend(sample.memory.rates.iter().map(|rate| float(*rate, 1)));
    cells.join(",")
}

fn disk_rows(timestamp: f64, sample: &Sample) -> Vec<String> {
    sample
        .disks
        .iter()
        .map(|d| {
            [
                format!("{timestamp:.3}"),
                d.device.clone(),
                float(Some(d.r_s), 2),
                float(Some(d.w_s), 2),
                float(Some(d.rkb_s), 2),
                float(Some(d.wkb_s), 2),
                float(Some(d.r_await_ms), 3),
                float(Some(d.w_await_ms), 3),
                float(Some(d.rareq_kb), 2),
                float(Some(d.wareq_kb), 2),
                float(Some(d.aqu_sz), 3),
                float(Some(d.util_pct), 2),
                float(d.d_s, 2),
                float(d.dkb_s, 2),
                float(d.f_s, 2),
                float(d.f_await_ms, 3),
            ]
            .join(",")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::sysmon::delta::{
        Activity, CpuShares, DiskSample, MemorySample, PressureShares,
    };

    fn columns(line: &str) -> usize {
        line.split(',').count()
    }

    fn sample() -> Sample {
        let mut gauges = vec![None; MEM_GAUGES.len()];
        gauges[0] = Some(16_314_388);
        gauges[2] = Some(9_000_000);
        let mut rates = vec![None; VM_RATES.len()];
        rates[0] = Some(1234.56);
        Sample {
            interval_s: 1.0,
            cpu: Some(CpuShares {
                user: 50.0,
                system: 12.345,
                idle: 37.655,
                ..CpuShares::default()
            }),
            activity: Activity {
                ctxt_s: Some(2000.04),
                forks_s: None,
                procs_running: Some(4),
                procs_blocked: Some(0),
                load1: Some(1.5),
                load5: Some(1.0),
                load15: Some(0.5),
            },
            pressure_cpu: PressureShares {
                some: Some(3.0),
                full: None,
            },
            pressure_memory: PressureShares::default(),
            pressure_io: PressureShares {
                some: Some(25.0),
                full: Some(5.0),
            },
            memory: MemorySample { gauges, rates },
            disks: vec![
                DiskSample {
                    device: "nvme0n1".into(),
                    r_s: 100.0,
                    w_s: 200.0,
                    rkb_s: 800.0,
                    wkb_s: 3200.0,
                    r_await_ms: 0.5,
                    w_await_ms: 2.0,
                    rareq_kb: 8.0,
                    wareq_kb: 16.0,
                    aqu_sz: 0.45,
                    util_pct: 50.0,
                    d_s: Some(2.0),
                    dkb_s: Some(2048.0),
                    f_s: None,
                    f_await_ms: None,
                },
                DiskSample {
                    device: "sda".into(),
                    ..DiskSample::default()
                },
            ],
        }
    }

    #[test]
    fn rows_have_as_many_cells_as_their_header() {
        let sample = sample();
        assert_eq!(columns(&cpu_row(1.0, &sample)), columns(CPU_HEADER));
        assert_eq!(
            columns(&memory_row(1.0, &sample)),
            columns(&memory_header())
        );
        for row in disk_rows(1.0, &sample) {
            assert_eq!(columns(&row), columns(DISK_HEADER));
        }
        // A sample without CPU time still fills its row.
        let idle = Sample {
            cpu: None,
            ..sample
        };
        assert_eq!(columns(&cpu_row(1.0, &idle)), columns(CPU_HEADER));
    }

    #[test]
    fn missing_figures_are_empty_cells() {
        let sample = sample();
        assert_eq!(
            cpu_row(1793005977.4123, &sample),
            "1793005977.412,50.00,0.00,12.35,0.00,0.00,0.00,0.00,0.00,0.00,37.66,\
2000.0,,4,0,1.50,1.00,0.50,3.00,,,,25.00,5.00"
        );
        let memory = memory_row(1793005977.4123, &sample);
        assert!(
            memory.starts_with("1793005977.412,16314388,,9000000,,"),
            "{memory}"
        );
        assert!(memory.ends_with(",1234.6,,,,,,"), "{memory}");
        assert_eq!(
            disk_rows(1793005977.4123, &sample)[0],
            "1793005977.412,nvme0n1,100.00,200.00,800.00,3200.00,0.500,2.000,8.00,16.00,\
0.450,50.00,2.00,2048.00,,"
        );
    }

    #[test]
    fn the_memory_header_keeps_the_kernel_names() {
        let header = memory_header();
        assert!(header.starts_with("timestamp,MemTotal,MemFree,MemAvailable,"));
        assert!(header.ends_with(",pgfault_s,pgmajfault_s,oom_kill_s"));
    }

    #[test]
    fn the_files_are_created_with_headers_and_grow_by_sample() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = Writer::create(dir.path(), "db1").unwrap();
        writer.write(10.0, &sample()).unwrap();
        writer.write(11.0, &sample()).unwrap();

        let read = |kind: &str| {
            std::fs::read_to_string(dir.path().join(format!("pgmeter-system-db1-{kind}.csv")))
                .unwrap()
        };
        assert_eq!(read("cpu").lines().count(), 3);
        assert_eq!(read("memory").lines().count(), 3);
        // Two devices per sample.
        assert_eq!(read("disk").lines().count(), 5);
        assert!(read("cpu").starts_with("timestamp,user_pct,"));
        assert!(read("disk").starts_with("timestamp,device,r_s,"));
    }
}
