mod args;
mod executor;
mod spec;

use args::{Action, OutputFormat};
use executor::error::Error;
use executor::Executor;

fn main() {
    if let Err(error) = run() {
        executor::exit_with_error(&error);
    }
}

/// The whole program as a fallible pipeline: every failure, from argument
/// resolution to the report, comes back here as an `Error`, and `main` is the
/// one place that prints it and sets the exit status.
fn run() -> Result<(), Error> {
    // Parse command arguments
    let env = args::CliArgs::new(args::get_os_username());

    // `spec` subcommands operate purely on the embedded templates and never
    // touch the database, so handle them before resolving a DSN or --spec.
    match &env.action {
        Action::SpecList => {
            spec::print_list();
            return Ok(());
        }
        Action::SpecNew {
            benchmark,
            output,
            force,
        } => {
            let path = spec::new(benchmark, output, *force).map_err(Error::Spec)?;
            println!("Created {}", path.display());
            return Ok(());
        }
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
            Executor::new(dsn, env.sslmode, spec_file)?
                .run_benchmark(env.run_args)?
                .aggregate_data()?
                .generate_report()?
                .print_results(output_format)?;
        }
        Action::Init => {
            let init = env.init_args;
            Executor::new(dsn, env.sslmode, spec_file)?
                .init_db_schema()?
                .exec_post_create_table_script(init.post_create_table)?
                .load_data(init.scalefactor, init.jobs, init.seed)?
                .add_primary_keys(init.jobs)?
                .add_foreign_keys(init.jobs, init.no_fkey)?
                .add_indexes(init.jobs)?
                .vacuum(init.jobs)?
                .checkpoint()?;
        }
        // Spec subcommands are handled and returned above.
        Action::SpecNew { .. } | Action::SpecList => unreachable!(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Cargo exposes the manifest's package metadata at compile time. These
    // checks pin the fields a published release needs so an accidental
    // removal fails the build's tests rather than `cargo publish`.
    #[test]
    fn package_metadata_is_release_ready() {
        assert_eq!(env!("CARGO_PKG_LICENSE"), "PostgreSQL");
        assert!(
            !env!("CARGO_PKG_DESCRIPTION").is_empty(),
            "description is required"
        );
        assert!(!env!("CARGO_PKG_AUTHORS").is_empty(), "authors is required");
        assert_eq!(
            env!("CARGO_PKG_REPOSITORY"),
            "https://github.com/julmon/pgmeter"
        );
        assert!(
            !env!("CARGO_PKG_HOMEPAGE").is_empty(),
            "homepage is required"
        );
        assert_eq!(env!("CARGO_PKG_README"), "README.md");
    }

    // CHANGELOG.md must exist and carry either an "Unreleased" section or an
    // entry for the version being built, so a release cannot ship without
    // release notes.
    #[test]
    fn changelog_has_an_entry_for_this_version() {
        let changelog = include_str!("../CHANGELOG.md");
        let version_heading = format!("## [{}]", env!("CARGO_PKG_VERSION"));
        assert!(
            changelog.contains("## [Unreleased]") || changelog.contains(&version_heading),
            "CHANGELOG.md needs a `## [Unreleased]` or {version_heading:?} section"
        );
    }

    // The MSRV is declared once in Cargo.toml and repeated in the README's
    // Requirements section; both must agree.
    #[test]
    fn readme_states_the_declared_rust_version() {
        let msrv = env!("CARGO_PKG_RUST_VERSION");
        assert!(!msrv.is_empty(), "package.rust-version must be set");
        let readme = include_str!("../README.md");
        let line = format!("- Rust {msrv} or later");
        assert!(
            readme.contains(&line),
            "README.md Requirements must contain {line:?}"
        );
    }
}
