extern crate clap;

use clap::{Arg, ArgAction, ColorChoice, Command};
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
    // Action: run, init, etc...
    pub action: String,
    // run arguments
    pub run_args: RunArgs,
    // init arguments
    pub init_args: InitArgs,
    // Path to YAML benchmark specification file
    pub spec_file: Option<String>,
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
        InitArgs {scalefactor: 0, jobs: 0, no_fkey: false, post_create_table: None}
    }
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
            .default_value(&username);

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
            .value_name("FILE");

        // Sub-commands
        // init --spec <FILE> <OPTIONS>
        let init = Command::new("init")
            .about("Initialize benchmark from a YAML specification file")
            .arg_required_else_help(true)
            .arg(spec_option.clone())
            .arg(scalefactor_option)
            .arg(jobs_option)
            .arg(no_fkey_option)
            .arg(post_create_table_option);

        // run --spec <FILE> <OPTIONS>
        let run = Command::new("run")
            .about("Run benchmark from a YAML specification file")
            .arg_required_else_help(true)
            .arg(spec_option)
            .arg(client_option)
            .arg(time_option)
            .arg(rampup_option);

        // Basic app information
        let cmd = Command::new("pgmtr")
            .version(env!("CARGO_PKG_VERSION"))
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
            // Target the full scale range: min defaults to 1, max to 0 (auto-detected at run time).
            let min_id = 1;
            let max_id = 0;

            RunArgs {client, time, rampup, min_id, max_id}
        }

        // Extract subcommand options
        let (run_args, init_args, action, spec_file) = match matches.subcommand_name() {
            Some("init") => {
                let init_m = matches.subcommand_matches("init").unwrap();
                let init_args = parse_init_args(init_m);
                let spec = init_m.get_one::<String>("spec").cloned();

                (RunArgs::empty(), init_args, "init".to_string(), spec)
            },
            Some("run") => {
                let run_m = matches.subcommand_matches("run").unwrap();
                let run_args = parse_run_args(run_m);
                let spec = run_m.get_one::<String>("spec").cloned();

                (run_args, InitArgs::empty(), "run".to_string(), spec)
            },
            _ => (RunArgs::empty(), InitArgs::empty(), "undefined".to_string(), None),
        };

        Ok(
            PgMtrArgs {
                host: host.to_string(),
                port: port,
                username: username.to_string(),
                password: password,
                dbname: dbname.to_string(),
                action: action,
                run_args: run_args,
                init_args: init_args,
                spec_file: spec_file,
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
