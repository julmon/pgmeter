//! End-to-end tests against a live PostgreSQL server, driving the built
//! `pgmeter` binary the way a user does: `init`, then `run`, then checking
//! what landed on stdout and in the result directory.
//!
//! They need `PGMETER_TEST_DSN` (a libpq URL) and are skipped, not failed,
//! when it is unset. CI's e2e job sets it against PostgreSQL 12 and 18.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Connection settings for the binary, derived from PGMETER_TEST_DSN, or None
/// to skip.
fn connection_env() -> Option<Vec<(&'static str, String)>> {
    let dsn = std::env::var("PGMETER_TEST_DSN").ok()?;
    let config: postgres::Config = dsn.parse().expect("PGMETER_TEST_DSN must be a libpq URL");
    let host = match config.get_hosts().first() {
        Some(postgres::config::Host::Tcp(h)) => h.clone(),
        #[cfg(unix)]
        Some(postgres::config::Host::Unix(p)) => p.display().to_string(),
        None => "localhost".to_string(),
    };
    let mut env = vec![
        ("PGHOST", host),
        (
            "PGPORT",
            config
                .get_ports()
                .first()
                .copied()
                .unwrap_or(5432)
                .to_string(),
        ),
    ];
    if let Some(user) = config.get_user() {
        env.push(("PGUSER", user.to_string()));
    }
    if let Some(password) = config.get_password() {
        env.push(("PGPASSWORD", String::from_utf8_lossy(password).into_owned()));
    }
    if let Some(db) = config.get_dbname() {
        env.push(("PGDATABASE", db.to_string()));
    }
    Some(env)
}

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join(name)
}

/// Fresh working directory per test: `run` writes its result directory into
/// the current directory.
fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pgmeter-e2e-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn pgmeter(env: &[(&str, String)], cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pgmeter"));
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn pgmeter")
}

fn assert_success(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The single `pgmeter-*` result directory inside `cwd`.
fn result_dir(cwd: &Path) -> PathBuf {
    let mut dirs: Vec<PathBuf> = fs::read_dir(cwd)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("pgmeter-")
        })
        .collect();
    assert_eq!(
        dirs.len(),
        1,
        "expected one result directory, found {dirs:?}"
    );
    dirs.pop().unwrap()
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn custom_simple_init_then_run_with_json_output() {
    let Some(env) = connection_env() else {
        eprintln!("PGMETER_TEST_DSN not set, skipping");
        return;
    };
    let cwd = workdir("custom-simple");
    let spec = sample("custom-simple");

    let init = pgmeter(
        &env,
        &cwd,
        &[
            "init",
            "--spec",
            spec.to_str().unwrap(),
            "-s",
            "3",
            "-j",
            "2",
        ],
    );
    assert_success(&init, "init custom-simple");

    let run = pgmeter(
        &env,
        &cwd,
        &[
            "run",
            "--spec",
            spec.to_str().unwrap(),
            "-c",
            "2",
            "-T",
            "2",
            "--format",
            "json",
        ],
    );
    assert_success(&run, "run custom-simple");

    // --format json implies quiet: stdout is exactly one JSON document.
    let doc: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("stdout must be a single JSON document");
    assert_eq!(doc["throughput_unit"], "TPM");
    assert!(doc["duration_s"].as_u64().unwrap() >= 2);
    let names: Vec<&str> = doc["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["read-product", "place-order", "list-recent-orders"]);
    let committed: u64 = doc["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["committed"].as_u64().unwrap())
        .sum();
    assert!(committed > 0, "no transaction committed in 2 s: {doc}");
    for t in doc["transactions"].as_array().unwrap() {
        assert_eq!(t["errors"], 0, "{t}");
        assert!(t["latency_ms"]["p99"].as_f64().unwrap() > 0.0, "{t}");
    }

    // Result directory contents.
    let dir = result_dir(&cwd);
    let names = file_names(&dir);
    for expected in ["report.html", "run.json", "transaction.log", "error.log"] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} missing in {names:?}"
        );
    }
    assert!(names
        .iter()
        .any(|n| n.starts_with("pgmeter-stats-") && n.ends_with(".csv")));
    assert!(names
        .iter()
        .any(|n| n.starts_with("pgmeter-response-time-")));
    let run_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run_json["clients"], 2);
    assert_eq!(run_json["duration_s"], 2);
    assert!(run_json["rampup_end_ts"].is_i64(), "{run_json}");
    assert!(fs::read_to_string(dir.join("error.log"))
        .unwrap()
        .is_empty());
    let report = fs::read_to_string(dir.join("report.html")).unwrap();
    assert!(report.contains("const DATA = {"));
    assert!(report.contains("my-app-benchmark"));

    fs::remove_dir_all(&cwd).ok();
}

#[test]
fn pgm_b_init_then_run_with_csv_output() {
    let Some(env) = connection_env() else {
        eprintln!("PGMETER_TEST_DSN not set, skipping");
        return;
    };
    let cwd = workdir("pgm-b");
    let spec = sample("pgm-b");

    assert_success(
        &pgmeter(
            &env,
            &cwd,
            &[
                "init",
                "--spec",
                spec.to_str().unwrap(),
                "-s",
                "2",
                "-j",
                "2",
            ],
        ),
        "init pgm-b",
    );
    let run = pgmeter(
        &env,
        &cwd,
        &[
            "run",
            "--spec",
            spec.to_str().unwrap(),
            "-c",
            "2",
            "-T",
            "2",
            "-r",
            "1",
            "--format",
            "csv",
        ],
    );
    assert_success(&run, "run pgm-b");

    let csv = String::from_utf8(run.stdout).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 2, "header plus one transaction row: {csv}");
    assert!(
        lines[0].starts_with("transaction,committed,errors,error_rate,TPS,"),
        "{csv}"
    );
    let fields: Vec<&str> = lines[1].split(',').collect();
    assert_eq!(fields[0], "Debit/credit");
    assert!(fields[1].parse::<u64>().unwrap() > 0, "{csv}");
    assert_eq!(fields[2], "0");

    // Ramp-up bookkeeping: the boundary is recorded and the workload window
    // excludes the ramp-up second.
    let dir = result_dir(&cwd);
    let run_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run_json["rampup_s"], 1);
    let total = run_json["total_time_ms"].as_u64().unwrap();
    let rampup = run_json["rampup_time_ms"].as_u64().unwrap();
    assert!(rampup >= 1000 && rampup < total, "{run_json}");
    let workload = run_json["workload_secs"].as_f64().unwrap();
    assert!(
        (workload - (total - rampup) as f64 / 1000.0).abs() < 1e-6,
        "{run_json}"
    );

    fs::remove_dir_all(&cwd).ok();
}

#[test]
fn failing_transactions_are_counted_and_the_run_completes() {
    let Some(env) = connection_env() else {
        eprintln!("PGMETER_TEST_DSN not set, skipping");
        return;
    };
    let cwd = workdir("failing");
    let spec_dir = cwd.join("spec");
    fs::create_dir_all(&spec_dir).unwrap();
    // One transaction that always fails inside a transaction block (B3: the
    // connection must recover), one that always succeeds, in both modes.
    fs::write(
        spec_dir.join("spec.yaml"),
        r#"
name: e2e-failing
scaling:
  unit: thing
  max_id_query: "SELECT MAX(id) FROM e2e_thing"
schema:
  tables:
    - name: e2e_thing
      columns: [{name: id, type: int}]
      primary_key: [id]
load:
  - table: e2e_thing
    copy_columns: [id]
    rows_per_id: 1
    row: "{{id}}\n"
transactions:
  - id: 1
    name: simple-fails
    weight: 25
    sql: |
      BEGIN;
      SELECT 1 / 0;
      COMMIT;
  - id: 2
    name: simple-ok
    weight: 25
    bindings:
      tid: "rand_int(min_id, max_id)"
    sql: "SELECT id FROM e2e_thing WHERE id = {{tid}}"
  - id: 3
    name: script-fails
    weight: 25
    mode: script
    script: |
      begin()
      exec("SELECT 1 / 0")
      commit()
  - id: 4
    name: script-ok
    weight: 25
    mode: script
    script: |
      local n = query_one("SELECT count(*) FROM e2e_thing")
      if n < 1 then error("table empty") end
"#,
    )
    .unwrap();
    let spec = spec_dir.to_str().unwrap();

    assert_success(
        &pgmeter(&env, &cwd, &["init", "--spec", spec, "-s", "4"]),
        "init failing",
    );
    let run = pgmeter(
        &env,
        &cwd,
        &[
            "run", "--spec", spec, "-c", "2", "-T", "2", "--format", "json",
        ],
    );
    assert_success(&run, "run failing");

    let doc: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    let tx = |name: &str| {
        doc["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} missing in {doc}"))
    };
    for failing in ["simple-fails", "script-fails"] {
        let t = tx(failing);
        assert_eq!(t["committed"], 0, "{t}");
        assert!(t["errors"].as_u64().unwrap() > 0, "{t}");
        assert_eq!(t["error_rate"], 100.0, "{t}");
    }
    // The failures must not poison the connection for the next transaction.
    for ok in ["simple-ok", "script-ok"] {
        let t = tx(ok);
        assert!(t["committed"].as_u64().unwrap() > 0, "{t}");
        assert_eq!(t["errors"], 0, "{t}");
    }

    // error.log: one record per line, each starting with a timestamp, and the
    // server message present for both modes (B11, B16).
    let dir = result_dir(&cwd);
    let errors = fs::read_to_string(dir.join("error.log")).unwrap();
    assert!(!errors.is_empty());
    for line in errors.lines() {
        let ts = line.split(' ').next().unwrap();
        assert!(
            ts.len() == 10 && ts.chars().all(|c| c.is_ascii_digit()),
            "bad error.log line: {line}"
        );
        assert!(
            line.contains("division by zero"),
            "server message lost: {line}"
        );
    }

    fs::remove_dir_all(&cwd).ok();
}
