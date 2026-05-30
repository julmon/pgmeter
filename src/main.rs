mod executor;
mod args;
mod spec;

use args::{Action, OutputFormat};

fn main() {
    // Parse command arguments
    let env = args::PgMtrArgs::new(args::get_os_username());

    // `spec` subcommands operate purely on the embedded templates and never
    // touch the database, so handle them before resolving a DSN or --spec.
    match &env.action {
        Action::SpecList => {
            spec::print_list();
            return;
        },
        Action::SpecNew { benchmark, output, force } => {
            match spec::new(benchmark, output, *force) {
                Ok(path) => println!("Created {}", path.display()),
                Err(e) => {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
            return;
        },
        _ => {}
    }

    let dsn = args::get_dsn(&env);

    let spec_file = env.spec_file.expect("--spec is required");

    // Run quietly when explicitly requested, or implicitly when the run results
    // are emitted in a machine-readable format so stdout carries only the data.
    let quiet = env.quiet
        || (matches!(env.action, Action::Run) && env.output_format != OutputFormat::Table);
    executor::set_quiet(quiet);

    match env.action {
        Action::Run => {
            let output_format = env.output_format;
            executor::Executor::new(dsn, spec_file)
                .run_benchmark(env.run_args)
                .aggregate_data()
                .generate_report()
                .print_results(output_format);
        },
        Action::Init => {
            executor::Executor::new(dsn, spec_file)
                .init_db_schema()
                .exec_post_create_table_script(env.init_args.post_create_table)
                .load_data(env.init_args.scalefactor, env.init_args.jobs)
                .add_primary_keys(env.init_args.jobs)
                .add_foreign_keys(env.init_args.jobs, env.init_args.no_fkey)
                .add_indexes(env.init_args.jobs)
                .vacuum(env.init_args.jobs)
                .checkpoint();
        },
        // Spec subcommands are handled and returned above.
        Action::SpecNew { .. } | Action::SpecList => unreachable!(),
    }
}
