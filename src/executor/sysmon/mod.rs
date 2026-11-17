//! System metrics of the hosts a benchmark runs on: CPU, memory and I/O,
//! sampled while the workload runs.
//!
//! A sampler script (`sampler.sh`) copies `/proc` to its standard output at a
//! fixed interval, on this machine or at the far end of an SSH connection.
//! Everything else happens here: `proto` cuts the stream into samples,
//! `parse` reads the `/proc` text into counters and gauges, and `delta` turns
//! two consecutive samples into rates and shares.

// The series are kept for the results and the report, which do not read
// them yet.

pub mod database;
pub mod delta;
pub mod devices;
pub mod parse;
pub mod proto;
pub mod target;
pub mod window;
pub mod writer;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

pub use database::Database;
use delta::Sample;
pub use devices::Device;
use proto::{Decoder, Event, Hello};
pub use target::{Spec, Target};
use writer::Writer;

/// The sampler, fed to `sh -s -- <interval>` on its standard input.
pub const SAMPLER_SH: &str = include_str!("sampler.sh");

/// How long a host has to deliver its first sample: the SSH connection
/// (`ConnectTimeout=10`), the login and one read of `/proc`.
const START_TIMEOUT: Duration = Duration::from_secs(15);

/// How much of a child's standard error is kept for the error messages.
const STDERR_KEPT: usize = 2000;

/// A monitored host, as it described itself with its first sample.
#[derive(Debug, Clone, PartialEq)]
pub struct HostInfo {
    /// The `--sysmon` value.
    pub target: String,
    /// The name of the host in the file names: `local`, or the host name it
    /// reported, made unique among the targets.
    pub label: String,
    pub host: String,
    pub kernel: String,
    pub cpus: u32,
    pub mem_total_kb: u64,
    pub interval_s: f64,
    /// PostgreSQL runs on this host: it was marked `db:`, or the server's
    /// address is one of its own, or, failing both, it is the first target.
    pub database: bool,
    /// The host's block devices and how they stack.
    pub devices: Vec<Device>,
}

/// The figures of one interval and when it ended, in Unix seconds on this
/// machine's clock: the clock of `transaction.log`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedSample {
    pub timestamp: f64,
    pub sample: Sample,
}

/// What a host's sampler delivered over the run.
#[derive(Debug, Clone, PartialEq)]
pub struct HostSeries {
    pub info: HostInfo,
    pub samples: Vec<TimedSample>,
    /// Why the sampler ended before the run did, when it did.
    pub truncated: Option<String>,
}

impl HostSeries {
    pub fn files(&self) -> [String; 3] {
        writer::file_names(&self.info.label)
    }

    /// The host's entry in `run.json`.
    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "target": self.info.target,
            "host": self.info.host,
            "kernel": self.info.kernel,
            "cpus": self.info.cpus,
            "mem_total_kb": self.info.mem_total_kb,
            "interval_s": self.info.interval_s,
            "database": self.info.database,
            "devices": self.info.devices.iter().map(Device::metadata).collect::<Vec<_>>(),
            "samples": self.samples.len(),
            "truncated": self.truncated.is_some(),
            "files": self.files(),
        })
    }
}

/// The first complete sample of a host: it answers, and this is what it is.
struct Greeting {
    hello: Hello,
    cpus: u32,
    mem_total_kb: u64,
}

struct ReaderResult {
    samples: Vec<TimedSample>,
    /// Set when the stream ended, or became unusable, before `stop`.
    ended: Option<String>,
}

struct Host {
    info: HostInfo,
    child: Child,
    stopping: Arc<AtomicBool>,
    reader: JoinHandle<ReaderResult>,
    stderr: Arc<Mutex<String>>,
    stderr_reader: JoinHandle<()>,
}

/// The samplers of a run, one child process and one reader thread per host.
/// Dropping the monitor kills the children.
pub struct Monitor {
    hosts: Vec<Host>,
}

impl Monitor {
    /// Starts a sampler on every target and returns once each has delivered
    /// its first sample. A target that does not is an error, and nothing is
    /// left running.
    pub fn start(
        targets: &[Target],
        interval_s: f64,
        dir: &Path,
        database: &Database,
    ) -> Result<Monitor, String> {
        Monitor::start_with(
            targets,
            interval_s,
            dir,
            database,
            &target::ssh_program(),
            START_TIMEOUT,
        )
    }

    fn start_with(
        targets: &[Target],
        interval_s: f64,
        dir: &Path,
        database: &Database,
        ssh_program: &[String],
        timeout: Duration,
    ) -> Result<Monitor, String> {
        // The paths PostgreSQL uses go ahead of the script, to every host:
        // which one is the database server is only known once all answered.
        let script = format!("{}{SAMPLER_SH}", database.script_prefix());
        struct Starting {
            target: Target,
            child: Child,
            stopping: Arc<AtomicBool>,
            reader: JoinHandle<ReaderResult>,
            writer_tx: Sender<Writer>,
            stderr: Arc<Mutex<String>>,
            stderr_reader: JoinHandle<()>,
        }

        // Kills what was started and names the target that failed, with what
        // its child last said on its standard error.
        fn abort(starting: Vec<Starting>, failed: usize, reason: String) -> String {
            let mut message = String::new();
            for (index, mut host) in starting.into_iter().enumerate() {
                host.stopping.store(true, Ordering::Relaxed);
                let _ = host.child.kill();
                let _ = host.child.wait();
                wait_briefly(&host.stderr_reader);
                if index == failed {
                    message = describe_failure(&host.target, &reason, &host.stderr);
                }
            }
            message
        }

        let (ready_tx, ready_rx) = unbounded::<(usize, Result<Greeting, String>)>();
        let mut starting: Vec<Starting> = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            let mut child = match target.command(interval_s, ssh_program).spawn() {
                Ok(child) => child,
                Err(error) => {
                    let program = match target {
                        Target::Local => "sh",
                        Target::Ssh { .. } => ssh_program[0].as_str(),
                    };
                    abort(starting, usize::MAX, String::new());
                    return Err(format!(
                        "system metrics of {target}: cannot start {program}: {error}"
                    ));
                }
            };
            // A child that is already gone (SSH refused) closes this pipe;
            // its standard error says why.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(script.as_bytes());
            }
            let stdout = child.stdout.take().expect("stdout is piped");
            let child_stderr = child.stderr.take().expect("stderr is piped");

            let stderr = Arc::new(Mutex::new(String::new()));
            let stderr_reader = {
                let kept = Arc::clone(&stderr);
                std::thread::spawn(move || keep_stderr(child_stderr, &kept))
            };
            let stopping = Arc::new(AtomicBool::new(false));
            let (writer_tx, writer_rx) = bounded::<Writer>(1);
            let reader = {
                let ready_tx = ready_tx.clone();
                let stopping = Arc::clone(&stopping);
                std::thread::spawn(move || {
                    read_samples(stdout, index, ready_tx, writer_rx, stopping)
                })
            };
            starting.push(Starting {
                target: target.clone(),
                child,
                stopping,
                reader,
                writer_tx,
                stderr,
                stderr_reader,
            });
        }
        drop(ready_tx);

        // Every host must answer before the deadline.
        let deadline = Instant::now() + timeout;
        let mut greetings: Vec<Option<Greeting>> = targets.iter().map(|_| None).collect();
        for _ in 0..targets.len() {
            match ready_rx.recv_deadline(deadline) {
                Ok((index, Ok(greeting))) => greetings[index] = Some(greeting),
                Ok((index, Err(reason))) => return Err(abort(starting, index, reason)),
                Err(_) => {
                    let silent = greetings.iter().position(Option::is_none).unwrap_or(0);
                    let reason = format!("no sample within {} s", timeout.as_secs_f64());
                    return Err(abort(starting, silent, reason));
                }
            }
        }

        // Name the hosts, create their files, and let the readers go on.
        let mut labels: Vec<String> = Vec::new();
        let mut infos: Vec<HostInfo> = Vec::new();
        let greetings: Vec<Greeting> = greetings
            .into_iter()
            .map(|greeting| greeting.expect("every host has answered"))
            .collect();
        let hellos: Vec<&Hello> = greetings.iter().map(|greeting| &greeting.hello).collect();
        let database_host = database.host(targets, &hellos);
        for (index, (target, greeting)) in targets.iter().zip(greetings).enumerate() {
            let wanted = match target {
                Target::Local => "local".to_string(),
                Target::Ssh { .. } if greeting.hello.host.is_empty() => {
                    file_label(&target.to_string())
                }
                Target::Ssh { .. } => file_label(&greeting.hello.host),
            };
            let label = unique_label(wanted, &labels);
            labels.push(label.clone());
            infos.push(HostInfo {
                database: index == database_host,
                devices: devices::topology(&greeting.hello, index == database_host),
                target: target.to_string(),
                label,
                host: greeting.hello.host,
                kernel: greeting.hello.kernel,
                cpus: greeting.cpus,
                mem_total_kb: greeting.mem_total_kb,
                interval_s,
            });
        }
        let mut writers = Vec::new();
        for info in &infos {
            match Writer::create(dir, &info.label) {
                Ok(writer) => writers.push(writer),
                Err(error) => {
                    abort(starting, usize::MAX, String::new());
                    return Err(format!(
                        "system metrics of {}: cannot create the files in {}: {error}",
                        info.target,
                        dir.display()
                    ));
                }
            }
        }

        let hosts = starting
            .into_iter()
            .zip(infos)
            .zip(writers)
            .map(|((host, info), writer)| {
                // The reader waits for its writer; it cannot have gone away.
                let _ = host.writer_tx.send(writer);
                Host {
                    info,
                    child: host.child,
                    stopping: host.stopping,
                    reader: host.reader,
                    stderr: host.stderr,
                    stderr_reader: host.stderr_reader,
                }
            })
            .collect();
        Ok(Monitor { hosts })
    }

    /// The hosts being monitored, in the order of the targets.
    #[cfg(test)]
    pub fn hosts(&self) -> impl Iterator<Item = &HostInfo> {
        self.hosts.iter().map(|host| &host.info)
    }

    /// Stops the samplers and returns what each host delivered.
    pub fn stop(mut self) -> Vec<HostSeries> {
        std::mem::take(&mut self.hosts)
            .into_iter()
            .map(|mut host| {
                host.stopping.store(true, Ordering::Relaxed);
                let _ = host.child.kill();
                let _ = host.child.wait();
                let result = host.reader.join().unwrap_or_else(|_| ReaderResult {
                    samples: Vec::new(),
                    ended: Some("the reader thread panicked".to_string()),
                });
                wait_briefly(&host.stderr_reader);
                let truncated = result.ended.map(|reason| {
                    let said = last_words(&host.stderr);
                    if said.is_empty() {
                        reason
                    } else {
                        format!("{reason}: {said}")
                    }
                });
                HostSeries {
                    info: host.info,
                    samples: result.samples,
                    truncated,
                }
            })
            .collect()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        for host in &mut self.hosts {
            host.stopping.store(true, Ordering::Relaxed);
            let _ = host.child.kill();
            let _ = host.child.wait();
        }
    }
}

/// The reader thread of one host: decodes the child's output, announces the
/// first sample, then turns each following one into a row of the files and
/// an entry of the series, until the stream ends.
fn read_samples(
    stdout: ChildStdout,
    index: usize,
    ready_tx: Sender<(usize, Result<Greeting, String>)>,
    writer_rx: Receiver<Writer>,
    stopping: Arc<AtomicBool>,
) -> ReaderResult {
    let mut reader = BufReader::new(stdout);
    let mut decoder = Decoder::new();
    let mut hello = Hello::default();
    let mut previous: Option<parse::Parsed> = None;
    let mut writer: Option<Writer> = None;
    let mut samples = Vec::new();
    let mut failure: Option<String> = None;
    let mut line = Vec::new();

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                failure = Some(format!("cannot read the sampler's output: {error}"));
                break;
            }
        }
        let raw = match decoder.feed(&String::from_utf8_lossy(&line)) {
            Ok(Some(Event::Hello(h))) => {
                hello = h;
                continue;
            }
            Ok(Some(Event::Sample(raw))) => raw,
            Ok(None) => continue,
            Err(message) => {
                failure = Some(message);
                break;
            }
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let current = match parse::parse_sample(&raw) {
            Ok(parsed) => parsed,
            // A host whose first sample cannot be read is refused; later, one
            // unreadable sample is one missing point.
            Err(message) if previous.is_none() => {
                failure = Some(message);
                break;
            }
            Err(_) => continue,
        };

        match (&previous, writer.as_mut()) {
            (Some(before), Some(writer)) => {
                if let Some(sample) = delta::delta(before, &current, &hello.blockdevs) {
                    if let Err(error) = writer.write(timestamp, &sample) {
                        failure = Some(format!("cannot write the system metrics: {error}"));
                        break;
                    }
                    samples.push(TimedSample { timestamp, sample });
                }
            }
            _ => {
                // The first sample is the baseline of the first interval.
                let greeting = Greeting {
                    hello: hello.clone(),
                    cpus: current.stat.cpus,
                    mem_total_kb: current.meminfo.get("MemTotal").copied().unwrap_or(0),
                };
                let _ = ready_tx.send((index, Ok(greeting)));
                match writer_rx.recv() {
                    Ok(created) => writer = Some(created),
                    // The start was called off because of another host.
                    Err(_) => {
                        return ReaderResult {
                            samples,
                            ended: None,
                        }
                    }
                }
            }
        }
        previous = Some(current);
    }

    if writer.is_none() {
        let reason =
            failure.unwrap_or_else(|| "the sampler ended before its first sample".to_string());
        let _ = ready_tx.send((index, Err(reason)));
        return ReaderResult {
            samples,
            ended: None,
        };
    }
    let ended = if stopping.load(Ordering::Relaxed) {
        None
    } else {
        Some(failure.unwrap_or_else(|| "the sampler stopped".to_string()))
    };
    ReaderResult { samples, ended }
}

/// Gives a standard-error reader the time to see the end of its stream. It
/// is not joined: a process the child left behind (an SSH connection master)
/// can keep the stream open long after the child is gone.
fn wait_briefly(reader: &JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_millis(500);
    while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Drains a child's standard error, keeping its end.
fn keep_stderr(mut stderr: impl Read, kept: &Mutex<String>) {
    let mut buffer = [0u8; 1024];
    while let Ok(n) = stderr.read(&mut buffer) {
        if n == 0 {
            break;
        }
        let mut text = kept.lock().unwrap_or_else(|e| e.into_inner());
        text.push_str(&String::from_utf8_lossy(&buffer[..n]));
        if text.len() > STDERR_KEPT {
            let mut cut = text.len() - STDERR_KEPT;
            while !text.is_char_boundary(cut) {
                cut += 1;
            }
            text.drain(..cut);
        }
    }
}

/// What a child said on its standard error, on one line.
fn last_words(stderr: &Mutex<String>) -> String {
    stderr
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn describe_failure(target: &Target, reason: &str, stderr: &Mutex<String>) -> String {
    let said = last_words(stderr);
    if said.is_empty() {
        format!("system metrics of {target}: {reason}")
    } else {
        format!("system metrics of {target}: {reason}: {said}")
    }
}

/// A host name as part of a file name: letters, digits, `-`, `_` and `.`
/// are kept, without a leading dot.
fn file_label(name: &str) -> String {
    let label: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match label.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None if label.is_empty() => "host".to_string(),
        None => label,
    }
}

/// `wanted`, or `wanted-2`, `wanted-3`... when other hosts have the name.
fn unique_label(wanted: String, taken: &[String]) -> String {
    if !taken.contains(&wanted) {
        return wanted;
    }
    (2..)
        .map(|n| format!("{wanted}-{n}"))
        .find(|label| !taken.contains(label))
        .expect("an unused suffix exists")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    use std::path::PathBuf;
    use std::time::Duration;

    use super::proto::{Decoder, Event};
    use super::{delta, file_label, parse, unique_label, Database, Monitor, Target, SAMPLER_SH};

    // The script against the kernel of the machine running the tests: what
    // it prints must decode, parse and yield a sample.
    #[test]
    fn the_sampler_script_yields_samples_on_this_machine() {
        let mut child = Command::new("sh")
            .args(["-s", "--", "0.2"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("sh starts");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(SAMPLER_SH.as_bytes())
            .unwrap();

        let mut decoder = Decoder::new();
        let mut hello = None;
        let mut samples = Vec::new();
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            match decoder.feed(&line.unwrap()).expect("a valid stream") {
                Some(Event::Hello(h)) => hello = Some(h),
                Some(Event::Sample(raw)) => {
                    samples.push(parse::parse_sample(&raw).expect("a parsable sample"));
                    if samples.len() == 2 {
                        break;
                    }
                }
                None => {}
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();

        let hello = hello.expect("a hello before the first sample");
        assert!(!hello.host.is_empty());
        assert!(!hello.kernel.is_empty());
        assert!(samples[0].stat.cpus >= 1);
        assert!(samples[0].meminfo["MemTotal"] > 0);

        let sample = delta::delta(&samples[0], &samples[1], &hello.blockdevs)
            .expect("the uptime advanced between the two samples");
        assert!(sample.interval_s > 0.1 && sample.interval_s < 5.0);
        let cpu = sample.cpu.expect("CPU time passed between the two samples");
        assert!((cpu.total() - 100.0).abs() < 1e-6, "{cpu:?}");
        assert!(sample.memory.gauge("MemTotal").unwrap() > 0);
    }

    /// A stand-in for `ssh`, run as `sh <script>` so that the file is never
    /// executed itself.
    fn shim(dir: &tempfile::TempDir, body: &str) -> Vec<String> {
        let path: PathBuf = dir.path().join("ssh-shim.sh");
        std::fs::write(&path, body).unwrap();
        vec!["sh".to_string(), path.display().to_string()]
    }

    fn remote() -> Vec<Target> {
        vec![Target::parse("postgres@db1:2222").unwrap()]
    }

    fn lines(dir: &tempfile::TempDir, name: &str) -> Vec<String> {
        std::fs::read_to_string(dir.path().join(name))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_local_host_is_sampled_into_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let monitor =
            Monitor::start(&[Target::Local], 0.1, dir.path(), &Database::default()).unwrap();
        let info = monitor.hosts().next().unwrap().clone();
        assert_eq!(info.label, "local");
        assert_eq!(info.target, "local");
        assert!(!info.host.is_empty() && !info.kernel.is_empty());
        assert!(info.cpus >= 1 && info.mem_total_kb > 0);

        std::thread::sleep(Duration::from_millis(700));
        let series = monitor.stop();
        assert_eq!(series.len(), 1);
        let host = &series[0];
        assert_eq!(host.truncated, None);
        assert!(host.samples.len() >= 2, "{} samples", host.samples.len());
        assert!(host
            .samples
            .windows(2)
            .all(|pair| pair[0].timestamp < pair[1].timestamp));

        let cpu = lines(&dir, "pgmeter-system-local-cpu.csv");
        assert_eq!(cpu.len(), host.samples.len() + 1);
        assert!(cpu[0].starts_with("timestamp,user_pct,"));
        let memory = lines(&dir, "pgmeter-system-local-memory.csv");
        assert_eq!(memory.len(), host.samples.len() + 1);
        assert!(dir.path().join("pgmeter-system-local-disk.csv").exists());

        let metadata = host.metadata();
        assert_eq!(metadata["target"], "local");
        assert_eq!(metadata["samples"], host.samples.len());
        assert_eq!(metadata["truncated"], false);
        assert_eq!(metadata["files"][0], "pgmeter-system-local-cpu.csv");
    }

    #[test]
    fn a_remote_host_is_sampled_through_the_ssh_command_and_named_after_itself() {
        let dir = tempfile::tempdir().unwrap();
        // Drops ssh's own arguments and the destination, then runs the
        // remote command here.
        let ssh = shim(
            &dir,
            "while [ \"$1\" != \"--\" ]; do shift; done\nshift; shift\nexec \"$@\"\n",
        );
        let targets = [remote()[0].clone(), remote()[0].clone(), Target::Local];
        let monitor = Monitor::start_with(
            &targets,
            0.1,
            dir.path(),
            &Database::default(),
            &ssh,
            Duration::from_secs(10),
        )
        .unwrap();
        let labels: Vec<String> = monitor.hosts().map(|info| info.label.clone()).collect();
        let host = file_label(&monitor.hosts().next().unwrap().host);
        assert_eq!(
            labels,
            [host.clone(), format!("{host}-2"), "local".to_string()]
        );

        std::thread::sleep(Duration::from_millis(500));
        let series = monitor.stop();
        assert_eq!(series.len(), 3);
        for host in &series {
            assert_eq!(host.truncated, None, "{}", host.info.label);
            assert!(!host.samples.is_empty(), "{}", host.info.label);
            assert!(dir.path().join(&host.files()[0]).exists());
        }
        assert_eq!(series[0].info.target, "postgres@db1:2222");
    }

    #[test]
    fn a_host_that_refuses_the_connection_fails_the_start_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = shim(
            &dir,
            "echo 'postgres@db1: Permission denied (publickey).' >&2\nexit 255\n",
        );
        let error = Monitor::start_with(
            &remote(),
            1.0,
            dir.path(),
            &Database::default(),
            &ssh,
            Duration::from_secs(10),
        )
        .err()
        .expect("the start fails");
        assert_eq!(
            error,
            "system metrics of postgres@db1:2222: the sampler ended before its first sample: \
postgres@db1: Permission denied (publickey)."
        );
        // No file is left for a host that never answered.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_host_that_is_not_linux_fails_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = shim(
            &dir,
            "cat >/dev/null\necho 'Last login: yesterday'\necho '@error unsupported operating system: Darwin'\nexit 1\n",
        );
        let error = Monitor::start_with(
            &remote(),
            1.0,
            dir.path(),
            &Database::default(),
            &ssh,
            Duration::from_secs(10),
        )
        .err()
        .expect("the start fails");
        assert_eq!(
            error,
            "system metrics of postgres@db1:2222: unsupported operating system: Darwin"
        );
    }

    #[test]
    fn a_silent_host_fails_the_start_at_the_deadline_and_stops_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = shim(&dir, "exec sleep 30\n");
        let targets = [Target::Local, remote()[0].clone()];
        let started = std::time::Instant::now();
        let error = Monitor::start_with(
            &targets,
            0.1,
            dir.path(),
            &Database::default(),
            &ssh,
            Duration::from_millis(600),
        )
        .err()
        .expect("the start fails");
        assert_eq!(
            error,
            "system metrics of postgres@db1:2222: no sample within 0.6 s"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_sampler_that_dies_mid_run_truncates_its_series() {
        let dir = tempfile::tempdir().unwrap();
        // Three samples, 100 ticks apart of which 50 in user mode, then the
        // connection drops.
        let ssh = shim(
            &dir,
            "cat >/dev/null
echo '@hello 1'; echo '@host db1.example.com'; echo '@kernel 5.14.0'; echo '@blockdevs'
i=0
while [ $i -lt 3 ]; do
  echo '@sample'
  echo \"/proc/uptime:$((100 + i)).00 0.00\"
  echo \"/proc/stat:cpu  $((i * 50)) 0 0 $((i * 50)) 0 0 0 0 0 0\"
  echo '/proc/stat:cpu0 0 0 0 0 0 0 0 0 0 0'
  echo '/proc/meminfo:MemTotal:       1000 kB'
  echo '@end'
  i=$((i + 1))
  sleep 0.1
done
echo 'Connection to db1 closed by remote host.' >&2
exit 255
",
        );
        let monitor = Monitor::start_with(
            &remote(),
            1.0,
            dir.path(),
            &Database::default(),
            &ssh,
            Duration::from_secs(10),
        )
        .unwrap();
        let info = monitor.hosts().next().unwrap().clone();
        assert_eq!(info.label, "db1.example.com");
        assert_eq!((info.cpus, info.mem_total_kb), (1, 1000));
        assert_eq!(info.kernel, "5.14.0");

        std::thread::sleep(Duration::from_millis(800));
        let series = monitor.stop();
        let host = &series[0];
        assert_eq!(
            host.truncated.as_deref(),
            Some("the sampler stopped: Connection to db1 closed by remote host.")
        );
        assert_eq!(host.samples.len(), 2);
        assert_eq!(host.samples[0].sample.cpu.unwrap().user, 50.0);
        assert_eq!(host.samples[0].sample.interval_s, 1.0);
        assert_eq!(host.metadata()["truncated"], true);
        assert_eq!(
            lines(&dir, "pgmeter-system-db1.example.com-cpu.csv").len(),
            3
        );
    }

    #[test]
    fn a_missing_ssh_program_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = vec!["/nonexistent/pgmeter-ssh".to_string()];
        let error = Monitor::start_with(
            &remote(),
            1.0,
            dir.path(),
            &Database::default(),
            &ssh,
            Duration::from_secs(10),
        )
        .err()
        .expect("the start fails");
        assert!(
            error.starts_with(
                "system metrics of postgres@db1:2222: cannot start /nonexistent/pgmeter-ssh: "
            ),
            "{error}"
        );
    }

    #[test]
    fn labels_are_safe_file_name_parts_and_unique() {
        assert_eq!(file_label("db1.example.com"), "db1.example.com");
        assert_eq!(file_label("a/b c"), "a_b_c");
        assert_eq!(file_label("../etc"), "_._etc");
        assert_eq!(file_label(""), "host");
        let taken = vec!["db1".to_string(), "db1-2".to_string()];
        assert_eq!(unique_label("db1".into(), &taken), "db1-3");
        assert_eq!(unique_label("db2".into(), &taken), "db2");
    }
}
