extern crate clap;

use clap::{Arg, ArgAction, ColorChoice, Command};
use std::ffi::OsString;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use users::{get_user_by_uid, get_current_uid};
use urlencoding::encode;

// clap value parser for --spec: accept either a benchmark directory (resolved
// to <dir>/spec.yaml) or a single spec file, and fail fast at parse time if
// neither exists instead of after connecting to the database.
fn spec_file_value(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if path.is_dir() {
        if path.join("spec.yaml").is_file() {
            Ok(path)
        } else {
            Err(format!("spec directory has no spec.yaml: '{}'", s))
        }
    } else if path.is_file() {
        Ok(path)
    } else {
        Err(format!("spec file not found: '{}'", s))
    }
}


// The sub-command the user invoked.
pub enum Action {
    Init,
    Run,
    // Scaffold an embedded benchmark spec to disk for editing.
    SpecNew { benchmark: String, output: String, force: bool },
    // List the benchmarks available to scaffold.
    SpecList,
}

// How `run` results are written to stdout.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum OutputFormat {
    Table,
    Json,
    Csv,
}

// clap value parser for --format.
fn output_format_value(s: &str) -> Result<OutputFormat, String> {
    match s.to_ascii_lowercase().as_str() {
        "table" => Ok(OutputFormat::Table),
        "json" => Ok(OutputFormat::Json),
        "csv" => Ok(OutputFormat::Csv),
        other => Err(format!("invalid output format '{}' (expected table, json or csv)", other)),
    }
}

// run sub-command arguments
pub struct RunArgs {
    // Number of concurrent client connected to the database
    pub client: u16,
    // Test duration, in second
    pub time: u16,
    // Rampup duration, in second
    pub rampup: u16,
    // Live progress reporting interval, in seconds (0 disables it)
    pub progress: u16,
}

// init sub-command arguments
pub struct InitArgs {
    // Database scale factor. In PGM-C context, this is the number of warehouses
    pub scalefactor: u32,
    // Number of concurrent jobs used to populate the database
    pub jobs: u32,
    // Do not create foreign keys
    pub no_fkey: bool,
    // Path to SQL script to execute after table creation
    pub post_create_table: Option<String>,
}

// CLI arguments
pub struct PgMtrArgs {
    // Database host address
    pub host: String,
    // TCP connection port
    pub port: u16,
    // Database username
    pub username: String,
    // Username password
    pub password: String,
    // Database name
    pub dbname: String,
    // Sub-command selected on the command line
    pub action: Action,
    // Suppress decorative progress output; emit only result data
    pub quiet: bool,
    // run arguments
    pub run_args: RunArgs,
    // init arguments
    pub init_args: InitArgs,
    // Path to YAML benchmark specification file
    pub spec_file: Option<String>,
    // Results output format (run only; Table for init)
    pub output_format: OutputFormat,
}

// Implementation of RunArgs::empty()
impl RunArgs {
    fn empty() -> Self {
        RunArgs {client: 0, time: 0, rampup: 0, progress: 0}
    }
}

// Implementation of InitArgs::empty()
impl InitArgs {
    fn empty() -> Self {
        InitArgs {scalefactor: 0, jobs: 0, no_fkey: false, post_create_table: None}
    }
}

impl PgMtrArgs {
    pub fn new(default_username: String) -> Self {
        Self::new_from(default_username, std::env::args_os().into_iter()).unwrap_or_else(|e| e.exit())
    }

    fn new_from<I, T>(default_username: String, args: I) -> Result<Self, clap::Error>
    where
        I: Iterator<Item = T>,
        T: Into<OsString> + Clone,
    {

        // Global options
        // Define the global --host/-H command line option
        let host_option = Arg::new("host")
            .long("host") // allow --host
            .action(ArgAction::Set)
            .global(true)
            .env("PGHOST")
            .short('H') // allow -H
            .help("Database server host or socket directory")
            .required(false)
            .value_name("HOSTNAME")
            .default_value("localhost");

        // Define the global --port/-p command line option
        let port_option = Arg::new("port")
            .long("port") // allow --port
            .action(ArgAction::Set)
            .global(true)
            .env("PGPORT")
            .short('p') // allow -p
            .help("Database server port")
            .required(false)
            .value_name("PORT")
            .value_parser(clap::value_parser!(u16).range(1..))
            .default_value("5432");

        // Define the global --username/-U command line option
        let username_option = Arg::new("username")
            .long("username") // allow --username
            .action(ArgAction::Set)
            .global(true)
            .short('U') // allow -U
            .env("PGUSER")
            .help("Database user name")
            .required(false)
            .value_name("USERNAME")
            .default_value(&default_username);

        // Define the global --dbname/-d command line option
        let dbname_option = Arg::new("dbname")
            .long("dbname") // allow --dbname
            .action(ArgAction::Set)
            .global(true)
            .short('d') // allow -d
            .env("PGDATABASE")
            .help("Database name to connect to")
            .required(false)
            .value_name("DBNAME")
            .default_value(&default_username);

        // run options
        // run: Define the --client/-c command line option
        let client_option = Arg::new("client")
            .long("client") // allow --client
            .action(ArgAction::Set)
            .short('c') // allow -c
            .help("Number of concurrent database clients")
            .required(false)
            .value_name("NUM")
            .value_parser(clap::value_parser!(u16).range(1..))
            .default_value("1");

        // run: Define the --time/-T command line option
        let time_option = Arg::new("time")
            .long("time") // allow --time
            .action(ArgAction::Set)
            .short('T') // allow -T
            .help("Duration of benchmark test in seconds")
            .required(false)
            .value_name("NUM")
            .value_parser(clap::value_parser!(u16).range(1..))
            .default_value("1");

        // run: Define the --rampup/-r command line option
        let rampup_option = Arg::new("rampup")
            .long("rampup") // allow --rampup
            .action(ArgAction::Set)
            .short('r') // allow -r
            .help("Duration of rampup stage in seconds")
            .required(false)
            .value_name("NUM")
            .value_parser(clap::value_parser!(u16))
            .default_value("0");

        // run: Define the --progress/-P command line option
        let progress_option = Arg::new("progress")
            .long("progress") // allow --progress
            .action(ArgAction::Set)
            .short('P') // allow -P
            .help("Print live progress every NUM seconds during the run (0 disables)")
            .required(false)
            .value_name("NUM")
            .value_parser(clap::value_parser!(u16))
            .default_value("0");

        // run: Define the --format command line option
        let format_option = Arg::new("format")
            .long("format")
            .action(ArgAction::Set)
            .help("Results output format")
            .required(false)
            .value_name("FORMAT")
            .value_parser(output_format_value)
            .default_value("table");

        // init: Define the --scalefactor/-s command line option
        let scalefactor_option = Arg::new("scalefactor")
            .long("scalefactor") // allow --scalefactor
            .action(ArgAction::Set)
            .short('s') // allow -s
            .help("Database scale factor")
            .required(false)
            .value_name("NUM")
            .value_parser(clap::value_parser!(u32).range(1..))
            .default_value("1");

        // init: Define the --jobs/-j command line option
        let jobs_option = Arg::new("jobs")
            .long("jobs") // allow --jobs
            .action(ArgAction::Set)
            .short('j') // allow -j
            .help("Number of threads used to populate the database")
            .required(false)
            .value_name("NUM")
            .value_parser(clap::value_parser!(u32).range(1..))
            .default_value("1");

        // Define the --quiet/-q flag (valid for both init and run). When set,
        // the decorative phase output is suppressed so only result data is
        // printed; for run with a non-table format it is implied.
        let quiet_option = Arg::new("quiet")
            .long("quiet") // allow --quiet
            .short('q') // allow -q
            .action(ArgAction::SetTrue)
            .help("Suppress progress output; print only result data");

        // init: Define the --no-fkey command line option
        let no_fkey_option = Arg::new("no_fkey")
            .long("no-fkey") // allow --no-fkey
            .action(ArgAction::SetTrue)
            .help("Do not create foreign keys");

        // init: Define the --post-create-table command line option
        let post_create_table_option = Arg::new("post_create_table")
            .long("post-create-table")
            .action(ArgAction::Set)
            .help("Path to SQL script to execute after table creation")
            .required(false)
            .value_name("FILE");

        // yaml: Define the --spec command line option
        let spec_option = Arg::new("spec")
            .long("spec")
            .action(ArgAction::Set)
            .help("Path to YAML benchmark specification file")
            .required(true)
            .value_name("FILE")
            .value_parser(spec_file_value);

        // Sub-commands
        // init --spec <FILE> <OPTIONS>
        let init = Command::new("init")
            .about("Initialize benchmark from a YAML specification file")
            .arg_required_else_help(true)
            .arg(spec_option.clone())
            .arg(scalefactor_option)
            .arg(jobs_option)
            .arg(no_fkey_option)
            .arg(post_create_table_option)
            .arg(quiet_option.clone());

        // run --spec <FILE> <OPTIONS>
        let run = Command::new("run")
            .about("Run benchmark from a YAML specification file")
            .arg_required_else_help(true)
            .arg(spec_option)
            .arg(client_option)
            .arg(time_option)
            .arg(rampup_option)
            .arg(progress_option)
            .arg(format_option)
            .arg(quiet_option);

        // spec new <BENCHMARK> [--output <DIR>] [--force]
        let spec_new = Command::new("new")
            .about("Scaffold an editable copy of an embedded benchmark spec")
            .arg_required_else_help(true)
            .arg(
                Arg::new("benchmark")
                    .action(ArgAction::Set)
                    .required(true)
                    .value_name("BENCHMARK")
                    .help("Embedded benchmark to scaffold (see `spec list`)"),
            )
            .arg(
                Arg::new("output")
                    .long("output")
                    .short('o')
                    .action(ArgAction::Set)
                    .default_value(".")
                    .value_name("DIR")
                    .help("Directory to write the benchmark folder into"),
            )
            .arg(
                Arg::new("force")
                    .long("force")
                    .action(ArgAction::SetTrue)
                    .help("Overwrite the destination if it already exists"),
            );

        // spec list
        let spec_list = Command::new("list").about("List the benchmarks available to scaffold");

        // spec <new|list>
        let spec = Command::new("spec")
            .about("Work with embedded benchmark specs")
            .arg_required_else_help(true)
            .subcommand_required(true)
            .subcommand(spec_new)
            .subcommand(spec_list);

        // Basic app information
        let cmd = Command::new("pgmeter")
            .version(env!("CARGO_PKG_VERSION"))
            .color(ColorChoice::Auto)
            .about("Postgres new generation benchmarking tool")
            .arg_required_else_help(true)
            .subcommand_required(true)
            .subcommand(init)
            .subcommand(run)
            .subcommand(spec);

        // Add in the arguments we want to parse
        let cmd = cmd
            .arg(host_option)
            .arg(port_option)
            .arg(username_option)
            .arg(dbname_option);

        // Extract the matches
        let matches = cmd.try_get_matches_from(args)?;

        // Extract the actual values
        let host = matches
            .get_one::<String>("host")
            .unwrap();
        let port = *matches
            .get_one::<u16>("port")
            .unwrap();
        let username = matches
            .get_one::<String>("username")
            .unwrap();
        let dbname = matches
            .get_one::<String>("dbname")
            .unwrap();

        // Helper function to parse init arguments
        fn parse_init_args(bm_m: &clap::ArgMatches) -> InitArgs {
            let scalefactor = *bm_m.get_one::<u32>("scalefactor").unwrap();
            let jobs = *bm_m.get_one::<u32>("jobs").unwrap();
            let no_fkey = bm_m.get_flag("no_fkey");
            let post_create_table = bm_m
                .get_one::<String>("post_create_table")
                .cloned();

            InitArgs {scalefactor, jobs, no_fkey, post_create_table}
        }

        // Helper function to parse run arguments
        fn parse_run_args(bm_m: &clap::ArgMatches) -> RunArgs {
            let client = *bm_m.get_one::<u16>("client").unwrap();
            let time = *bm_m.get_one::<u16>("time").unwrap();
            let rampup = *bm_m.get_one::<u16>("rampup").unwrap();
            let progress = *bm_m.get_one::<u16>("progress").unwrap();

            RunArgs {client, time, rampup, progress}
        }

        // Extract the validated --spec path as a string for downstream use.
        fn spec_path(bm_m: &clap::ArgMatches) -> Option<String> {
            bm_m.get_one::<PathBuf>("spec")
                .map(|p| p.to_string_lossy().into_owned())
        }

        // Extract subcommand options
        let (run_args, init_args, action, spec_file, output_format, quiet) = match matches.subcommand_name() {
            Some("init") => {
                let init_m = matches.subcommand_matches("init").unwrap();
                let init_args = parse_init_args(init_m);
                let spec = spec_path(init_m);
                let quiet = init_m.get_flag("quiet");

                (RunArgs::empty(), init_args, Action::Init, spec, OutputFormat::Table, quiet)
            },
            Some("run") => {
                let run_m = matches.subcommand_matches("run").unwrap();
                let run_args = parse_run_args(run_m);
                let spec = spec_path(run_m);
                let format = *run_m.get_one::<OutputFormat>("format").unwrap();
                let quiet = run_m.get_flag("quiet");

                (run_args, InitArgs::empty(), Action::Run, spec, format, quiet)
            },
            Some("spec") => {
                let spec_m = matches.subcommand_matches("spec").unwrap();
                let action = match spec_m.subcommand_name() {
                    Some("new") => {
                        let m = spec_m.subcommand_matches("new").unwrap();
                        Action::SpecNew {
                            benchmark: m.get_one::<String>("benchmark").unwrap().clone(),
                            output: m.get_one::<String>("output").unwrap().clone(),
                            force: m.get_flag("force"),
                        }
                    },
                    Some("list") => Action::SpecList,
                    // subcommand_required(true) on `spec` guarantees a match.
                    _ => unreachable!("a spec subcommand is required"),
                };

                (RunArgs::empty(), InitArgs::empty(), action, None, OutputFormat::Table, false)
            },
            // subcommand_required(true) guarantees a subcommand was matched.
            _ => unreachable!("a subcommand is required"),
        };

        // Resolve the password now that the connection target is known, so a
        // .pgpass lookup can match on host/port/database/user.
        let password = resolve_password(host, port, dbname, username);

        Ok(
            PgMtrArgs {
                host: host.to_string(),
                port: port,
                username: username.to_string(),
                password: password,
                dbname: dbname.to_string(),
                action: action,
                quiet: quiet,
                run_args: run_args,
                init_args: init_args,
                spec_file: spec_file,
                output_format: output_format,
            }
        )
    }
}

// Returns current username
pub fn get_os_username() -> String {
    let os_user = get_user_by_uid(get_current_uid()).unwrap();
    let os_username = os_user.name().to_str().unwrap();

    String::from(os_username)
}

// Returns the database connection string based on CLI args. Every user-supplied
// component is percent-encoded so special characters (e.g. '@', ':', '/' in a
// password or a socket-directory host path) don't corrupt the URL.
pub fn get_dsn(args: &PgMtrArgs) -> String {
    format!(
        "postgresql://{}:{}@{}:{}/{}",
        encode(&args.username),
        encode(&args.password),
        encode(&args.host),
        args.port,
        encode(&args.dbname),
    )
}

// Resolve the connection password. PGPASSWORD takes precedence; otherwise the
// password is looked up in the password file (PGPASSFILE or ~/.pgpass), matching
// the first line whose host:port:database:username fields match (with '*'
// wildcards). Returns an empty string when nothing matches.
fn resolve_password(host: &str, port: u16, dbname: &str, username: &str) -> String {
    if let Ok(password) = env::var("PGPASSWORD") {
        return password;
    }
    lookup_pgpass(host, port, dbname, username).unwrap_or_default()
}

// Location of the password file: PGPASSFILE if set, otherwise ~/.pgpass.
fn pgpass_path() -> Option<PathBuf> {
    if let Ok(file) = env::var("PGPASSFILE") {
        if !file.is_empty() {
            return Some(PathBuf::from(file));
        }
    }
    env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".pgpass"))
}

// Scan the password file for the first entry matching the connection target.
fn lookup_pgpass(host: &str, port: u16, dbname: &str, username: &str) -> Option<String> {
    let path = pgpass_path()?;
    let file = File::open(&path).ok()?;

    // Like libpq, refuse a file that is group- or world-accessible.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = file.metadata() {
            if meta.permissions().mode() & 0o077 != 0 {
                eprintln!(
                    "WARNING: password file \"{}\" has group or world access; permissions should be u=rw (0600) or less; ignoring",
                    path.display()
                );
                return None;
            }
        }
    }

    let port = port.to_string();
    for line in BufReader::new(file).lines().flatten() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = split_pgpass_line(line);
        if fields.len() != 5 {
            continue;
        }
        if pgpass_match(&fields[0], host)
            && pgpass_match(&fields[1], &port)
            && pgpass_match(&fields[2], dbname)
            && pgpass_match(&fields[3], username)
        {
            return Some(fields[4].clone());
        }
    }
    None
}

// A field matches when it is the '*' wildcard or equals the connection value.
fn pgpass_match(field: &str, value: &str) -> bool {
    field == "*" || field == value
}

// Split a .pgpass line into its 5 fields, honoring backslash escapes ('\:' and
// '\\') so colons and backslashes can appear inside a field (e.g. a password).
fn split_pgpass_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    current.push('\\');
                }
            }
            ':' => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn splits_pgpass_fields_with_escapes() {
        assert_eq!(
            split_pgpass_line("localhost:5432:db:user:secret"),
            vec!["localhost", "5432", "db", "user", "secret"],
        );
        // An escaped colon stays inside the password field, an escaped
        // backslash collapses to one.
        assert_eq!(
            split_pgpass_line(r"*:*:*:user:pa\:ss\\word"),
            vec!["*", "*", "*", "user", r"pa:ss\word"],
        );
    }

    #[test]
    fn matches_wildcards_and_exact_values() {
        assert!(pgpass_match("*", "anything"));
        assert!(pgpass_match("5432", "5432"));
        assert!(!pgpass_match("5432", "5433"));
    }

    #[test]
    fn lookup_returns_first_matching_entry() {
        let path = std::env::temp_dir().join(format!("pgmeter-pgpass-{}", std::process::id()));
        {
            let mut f = File::create(&path).unwrap();
            writeln!(f, "# comment").unwrap();
            writeln!(f, "otherhost:5432:db:user:nope").unwrap();
            writeln!(f, "localhost:5432:*:user:hunter2").unwrap();
            writeln!(f, "localhost:5432:db:user:later").unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::env::set_var("PGPASSFILE", &path);
        std::env::remove_var("PGPASSWORD");

        assert_eq!(
            resolve_password("localhost", 5432, "db", "user"),
            "hunter2".to_string(),
        );
        // No matching line -> empty password.
        assert_eq!(resolve_password("localhost", 5432, "db", "other"), String::new());

        std::env::remove_var("PGPASSFILE");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn spec_file_value_rejects_missing_path() {
        assert!(spec_file_value("/definitely/not/here.yaml").is_err());
    }
}
