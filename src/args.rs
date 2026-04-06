extern crate clap;

use clap::{Arg, ArgAction, ColorChoice, Command};
use clap::error::ErrorKind;
use std::ffi::OsString;
use std::env;
use users::{get_user_by_uid, get_current_uid};
use urlencoding::encode;


// run sub-command arguments
pub struct RunArgs {
    // Number of concurrent client connected to the database
    pub client: u16,
    // Test duration, in second
    pub time: u16,
    // Rampup duration, in second
    pub rampup: u16,
    // Minimum object ID value we want to hit during the benchmark. In TPCC context, the object ID is a warehouse id.
    // min_id and max_id define the target object interval.
    pub min_id: u32,
    // Max object ID value
    pub max_id: u32,
}

// init sub-command arguments
pub struct InitArgs {
    // Database scale factor. In TPC-C context, this is the number of warehouses
    pub scalefactor: u32,
    // Number of concurrent jobs used to populate the database
    pub jobs: u32,
    // Do not create foreign keys
    pub no_fkey: bool,
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
    // Action: run, init, etc...
    pub action: String,
    // Benchmark type: tpcc
    pub benchmark_type: String,
    // run arguments
    pub run_args: RunArgs,
    // init arguments
    pub init_args: InitArgs,
}

// Implementation of RunArgs::empty()
impl RunArgs {
    fn empty() -> Self {
        RunArgs {client: 0, time: 0, rampup: 0, min_id: 0, max_id: 0}
    }
}

// Implementation of InitArgs::empty()
impl InitArgs {
    fn empty() -> Self {
        InitArgs {scalefactor: 0, jobs: 0, no_fkey: false}
    }
}

// Parse and convert an argument string coming from clap to u16
fn parse_string_arg_to_u16(value: &str, error_msg: String) -> Result<u16, clap::Error> {
    let u16_value = match value.parse::<u16>() {
        Ok(v) => v,
        Err(_) => {
            return Err(
                clap::Error::raw(
                    ErrorKind::InvalidValue,
                    format!("{}: \"{}\"\n", error_msg, value.to_string())
                )
            )
        },
    };

    Ok(u16_value)
}

// Parse and convert an argument string coming from clap to u32
fn parse_string_arg_to_u32(value: &str, error_msg: String) -> Result<u32, clap::Error> {
    let u32_value = match value.parse::<u32>() {
        Ok(v) => v,
        Err(_) => {
            return Err(
                clap::Error::raw(
                    ErrorKind::InvalidValue,
                    format!("{}: \"{}\"\n", error_msg, value.to_string())
                )
            )
        },
    };

    Ok(u32_value)
}

impl PgMtrArgs {
    pub fn new(username: String, password: String) -> Self {
        Self::new_from(username, password, std::env::args_os().into_iter()).unwrap_or_else(|e| e.exit())
    }

    fn new_from<I, T>(username: String, password: String, args: I) -> Result<Self, clap::Error>
    where
        I: Iterator<Item = T>,
        T: Into<OsString> + Clone,
    {

        // Global options
        // Define the global --host/-H command line option
        let host_option = Arg::new("host")
            .long("host") // allow --host
            .action(ArgAction::Set)
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
            .env("PGPORT")
            .short('p') // allow -p
            .help("Database server port")
            .required(false)
            .value_name("PORT")
            .default_value("5432");

        // Define the global --username/-U command line option
        let username_option = Arg::new("username")
            .long("username") // allow --username
            .action(ArgAction::Set)
            .short('U') // allow -U
            .env("PGUSER")
            .help("Database user name")
            .required(false)
            .value_name("USERNAME")
            .default_value(&username);

        // Define the global --dbname/-d command line option
        let dbname_option = Arg::new("dbname")
            .long("dbname") // allow --dbname
            .action(ArgAction::Set)
            .short('d') // allow -d
            .env("PGDATABASE")
            .help("Database name to connect to")
            .required(false)
            .value_name("DBNAME")
            .default_value(&username);

        // run options
        // run: Define the --client/-c command line option
        let client_option = Arg::new("client")
            .long("client") // allow --client
            .action(ArgAction::Set)
            .short('c') // allow -c
            .help("Number of concurrent database clients")
            .required(false)
            .value_name("NUM")
            .default_value("1");

        // run: Define the --time/-T command line option
        let time_option = Arg::new("time")
            .long("time") // allow --time
            .action(ArgAction::Set)
            .short('T') // allow -T
            .help("Duration of benchmark test in seconds")
            .required(false)
            .value_name("NUM")
            .default_value("1");

        // run: Define the --rampup/-r command line option
        let rampup_option = Arg::new("rampup")
            .long("rampup") // allow --rampup
            .action(ArgAction::Set)
            .short('r') // allow -r
            .help("Duration of rampup stage in seconds")
            .required(false)
            .value_name("NUM")
            .default_value("0");

        // run: Define the --min-id command line option
        let min_id_option = Arg::new("min_id")
            .long("min-id") // allow --min-id
            .action(ArgAction::Set)
            .help("Interval's minimum ID value")
            .required(false)
            .value_name("NUM")
            .default_value("1");

        // run: Define the --max-id command line option
        let max_id_option = Arg::new("max_id")
            .long("max-id") // allow --max-id
            .action(ArgAction::Set)
            .help("Interval's maximum ID value. If set to 0, the value is automatically based on the scalefactor.")
            .required(false)
            .value_name("NUM")
            .default_value("0");

        // init: Define the --scalefactor/-s command line option
        let scalefactor_option = Arg::new("scalefactor")
            .long("scalefactor") // allow --scalefactor
            .action(ArgAction::Set)
            .short('s') // allow -s
            .help("Database scale factor")
            .required(false)
            .value_name("NUM")
            .default_value("1");

        // init: Define the --jobs/-j command line option
        let jobs_option = Arg::new("jobs")
            .long("jobs") // allow --jobs
            .action(ArgAction::Set)
            .short('j') // allow -j
            .help("Number of threads used to populated the database")
            .required(false)
            .value_name("NUM")
            .default_value("1");

        // init: Define the --no-fkey command line option
        let no_fkey_option = Arg::new("no_fkey")
            .long("no-fkey") // allow --no-fkey
            .action(ArgAction::SetTrue)
            .help("Do not create foreign keys");

        // Sub-commands
        // run tpcc <OPTIONS>
        let run_tpcc = Command::new("tpcc")
            .about("Run TPC-C-like benchmark")
            .arg(client_option.clone())
            .arg(time_option.clone())
            .arg(rampup_option.clone())
            .arg(min_id_option.clone())
            .arg(max_id_option.clone());

        // run tpcb <OPTIONS>
        let run_tpcb = Command::new("tpcb")
            .about("Run TPC-B benchmark (banking debit/credit)")
            .arg(client_option.clone())
            .arg(time_option.clone())
            .arg(rampup_option.clone())
            .arg(min_id_option.clone())
            .arg(max_id_option.clone());

        // run tpciot <OPTIONS>
        let run_tpciot = Command::new("tpciot")
            .about("Run TPCx-IoT-like benchmark")
            .arg(client_option)
            .arg(time_option)
            .arg(rampup_option)
            .arg(min_id_option)
            .arg(max_id_option);

        // init tpcc <OPTIONS>
        let init_tpcc = Command::new("tpcc")
            .about("Initialize TPC-C-like benchmark data")
            .arg(scalefactor_option.clone())
            .arg(jobs_option.clone())
            .arg(no_fkey_option.clone());

        // init tpcb <OPTIONS>
        let init_tpcb = Command::new("tpcb")
            .about("Initialize TPC-B benchmark data (banking debit/credit)")
            .arg(scalefactor_option.clone())
            .arg(jobs_option.clone())
            .arg(no_fkey_option.clone());

        // init tpciot <OPTIONS>
        let init_tpciot = Command::new("tpciot")
            .about("Initialize TPCx-IoT-like benchmark data")
            .arg(scalefactor_option)
            .arg(jobs_option)
            .arg(no_fkey_option);

        // init <SUBCOMMAND> <OPTIONS>
        let init = Command::new("init")
            .about("Initialize benchmark data")
            .arg_required_else_help(true)
            .subcommand_required(true)
            .subcommand(init_tpcc)
            .subcommand(init_tpcb)
            .subcommand(init_tpciot);

        // run <SUBCOMMAND> <OPTIONS>
        let run = Command::new("run")
            .about("Run benchmark")
            .arg_required_else_help(true)
            .subcommand_required(true)
            .subcommand(run_tpcc)
            .subcommand(run_tpcb)
            .subcommand(run_tpciot);

        // Basic app information
        let cmd = Command::new("pgmtr")
            .version("0.1.0")
            .color(ColorChoice::Never)
            .about("Postgres new generation benchmarking tool")
            .arg_required_else_help(true)
            .subcommand_required(true)
            .subcommand(init)
            .subcommand(run);

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
        let port_str = matches
            .get_one::<String>("port")
            .unwrap();
        let username = matches
            .get_one::<String>("username")
            .unwrap();
        let dbname = matches
            .get_one::<String>("dbname")
            .unwrap();

        // Convert port ton u16
        let port = parse_string_arg_to_u16(port_str, "invalid port number".to_string())?;

        // Helper function to parse init subcommand arguments
        fn parse_init_args(init_m: &clap::ArgMatches, benchmark_name: &str) -> Result<(RunArgs, InitArgs, String), clap::Error> {
            let (scalefactor, jobs, no_fkey) = match init_m.subcommand_matches(benchmark_name) {
                Some(bm_m) => {
                    let scalefactor_str = bm_m
                        .get_one::<String>("scalefactor")
                        .unwrap();
                    let jobs_str = bm_m
                        .get_one::<String>("jobs")
                        .unwrap();
                    let no_fkey = bm_m.get_flag("no_fkey");

                    let scalefactor = parse_string_arg_to_u32(scalefactor_str, "invalid scale factor number".to_string())?;
                    let jobs = parse_string_arg_to_u32(jobs_str, "invalid jobs number".to_string())?;

                    (scalefactor, jobs, no_fkey)
                },
                _ => (0, 0, false)
            };

            Ok((RunArgs::empty(), InitArgs {scalefactor, jobs, no_fkey}, benchmark_name.to_string()))
        }

        // Helper function to parse run subcommand arguments
        fn parse_run_args(run_m: &clap::ArgMatches, benchmark_name: &str) -> Result<(RunArgs, InitArgs, String), clap::Error> {
            let (client, time, rampup, min_id, max_id) = match run_m.subcommand_matches(benchmark_name) {
                Some(bm_m) => {
                    let client_str = bm_m.get_one::<String>("client").unwrap();
                    let time_str = bm_m.get_one::<String>("time").unwrap();
                    let rampup_str = bm_m.get_one::<String>("rampup").unwrap();
                    let min_id_str = bm_m.get_one::<String>("min_id").unwrap();
                    let max_id_str = bm_m.get_one::<String>("max_id").unwrap();

                    let client = parse_string_arg_to_u16(client_str, "invalid client number".to_string())?;
                    let time = parse_string_arg_to_u16(time_str, "invalid time value".to_string())?;
                    let rampup = parse_string_arg_to_u16(rampup_str, "invalid rampup value".to_string())?;
                    let min_id = parse_string_arg_to_u32(min_id_str, "invalid min ID value".to_string())?;
                    let max_id = parse_string_arg_to_u32(max_id_str, "invalid end id value".to_string())?;

                    (client, time, rampup, min_id, max_id)
                },
                _ => (0, 0, 0, 0, 0),
            };

            Ok((RunArgs {client, time, rampup, min_id, max_id}, InitArgs::empty(), benchmark_name.to_string()))
        }

        // Extract subcommand options
        let (run_args, init_args, action, benchmark_type) = match matches.subcommand_name() {
            Some("init") => {
                let init_m = matches.subcommand_matches("init").unwrap();
                let (run_args, init_args, benchmark_type) = match init_m.subcommand_name() {
                    Some("tpcc") => parse_init_args(init_m, "tpcc")?,
                    Some("tpcb") => parse_init_args(init_m, "tpcb")?,
                    Some("tpciot") => parse_init_args(init_m, "tpciot")?,
                    _ => (RunArgs::empty(), InitArgs::empty(), "undefined".to_string()),
                };

                (run_args, init_args, "init".to_string(), benchmark_type)
            },
            Some("run") => {
                let run_m = matches.subcommand_matches("run").unwrap();
                let (run_args, init_args, benchmark_type) = match run_m.subcommand_name() {
                    Some("tpcc") => parse_run_args(run_m, "tpcc")?,
                    Some("tpcb") => parse_run_args(run_m, "tpcb")?,
                    Some("tpciot") => parse_run_args(run_m, "tpciot")?,
                    _ => (RunArgs::empty(), InitArgs::empty(), "undefined".to_string()),
                };

                (run_args, init_args, "run".to_string(), benchmark_type)
            },
            _ => (RunArgs::empty(), InitArgs::empty(), "undefined".to_string(), "undefined".to_string()),
        };

        Ok(
            PgMtrArgs {
                host: host.to_string(),
                port: port,
                username: username.to_string(),
                password: password,
                dbname: dbname.to_string(),
                action: action,
                benchmark_type: benchmark_type,
                run_args: run_args,
                init_args: init_args,
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

// Returns the database connection string based on CLI args
pub fn get_dsn(args: &PgMtrArgs) -> String {
    format!("postgresql://{}:\"{}\"@{}:{}/{}", args.username, args.password, encode(&args.host), args.port, args.dbname)
}

// Returns the database password by looking up into multiple places: environment variable, .pgpass
pub fn get_pg_password() -> String {
    // Retreive the password from PGPASSWORD environment variable.
    let password = match env::var("PGPASSWORD") {
        Ok(p) => p,
        Err(_) => "".to_string(),
    };
    // TODO: implement .pgpass support
    // TODO: test password by opening a new connection to the DB, and ask for a new one if it fails
    // to connect (auth. error).
    password
}
