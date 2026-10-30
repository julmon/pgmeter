//! The `init` pipeline: schema creation, data loading with parallel jobs,
//! constraints, indexes, vacuum and checkpoint. Each step is a method of
//! `Executor` returning `Result<&mut Self, Error>` so `main` chains them.

use std::thread;
use std::thread::JoinHandle;
use std::time::Instant;

use postgres::Client;

use super::error::Error;
use super::{terminal, Executor};

impl Executor {
    // Initialize the database schema: create the tables of the benchmark
    pub fn init_db_schema(&mut self) -> Result<&mut Self, Error> {
        let command = "INIT";
        let message = "Executing database DDLs";

        terminal::start_msg(command, message);

        // New database connection
        let mut client = self.connect()?;

        // Load the corresponding benchmark client
        let benchmark_client = self.get_benchmark(0, 0, 0);

        // Initialize the database model/schema
        let duration_us = benchmark_client.initialize_schema(&mut client)?;
        let duration_ms = duration_us as f64 / 1000_f64;

        terminal::done_msg(duration_ms);

        Ok(self)
    }

    // Execute a post-create-table SQL script if provided
    pub fn exec_post_create_table_script(
        &mut self,
        script_path: Option<String>,
    ) -> Result<&mut Self, Error> {
        let path = match script_path {
            Some(p) => p,
            None => return Ok(self),
        };

        let command = "INIT";
        let message = format!("Executing post-create-table script: {}", path);

        terminal::start_msg(command, message.as_str());
        let start = std::time::Instant::now();

        // Read the SQL script file
        let sql = std::fs::read_to_string(&path)
            .map_err(|e| Error::io(format!("Failed to read script file '{}'", path), e))?;

        // Execute the script
        let mut client = self.connect()?;
        client.batch_execute(&sql)?;

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);

        Ok(self)
    }

    // Generate benchmark data
    pub fn load_data(
        &mut self,
        scalefactor: u32,
        n_jobs: u32,
        seed: Option<u64>,
    ) -> Result<&mut Self, Error> {
        // Load the corresponding benchmark client
        let benchmark_client = self.get_benchmark(scalefactor, 0, 0);

        let command = "INIT";
        let message = "Pre-loading operations";

        terminal::start_msg(command, message);

        // New database connection
        let mut client = self.connect()?;

        // Execute PreLoadData
        let duration_us = benchmark_client.pre_load_data(&mut client, seed)?;
        let duration_ms = duration_us as f64 / 1000_f64;

        terminal::done_msg(duration_ms);

        // Execute LoadData using multiple concurrent jobs
        let mut jobs = Vec::new();

        // Build the scalefactor ids matrix as follow (considering 3 jobs and 12 ids):
        // [1, 4, 7, 10]
        // [2, 5, 8, 11]
        // [3, 6, 9, 12]
        // We want to get one line per jobs and the ids balanced across the lines.
        let mut ids = Vec::with_capacity(n_jobs as usize);
        for _ in 0..n_jobs {
            ids.push(Vec::new());
        }
        for n in 1..=scalefactor {
            ids[(n % n_jobs) as usize].push(n);
        }

        let message2 = format!("Data loading using {} jobs", n_jobs);
        terminal::start_msg(command, message2.as_str());

        let start = Instant::now();

        for j in 1..=n_jobs {
            // Cloning values before passing them to the thread
            let job_ids = ids[(j - 1) as usize].clone();
            let dsn = self.dsn.clone();
            let tls = self.tls.clone();

            // Load the corresponding benchmark client
            let job_benchmark_client = self.get_benchmark(scalefactor, 0, 0);

            // Starting a new job into its dedicated thread
            let job = thread::spawn(move || -> Result<(), Error> {
                // New database connection
                let mut job_client = Client::connect(&dsn, tls)?;
                job_benchmark_client.load_data(&mut job_client, job_ids, seed)?;
                Ok(())
            });

            jobs.push(job);
        }

        // Wait for the end of all jobs, keeping the first failure.
        join_jobs(jobs)?;
        let duration_ms = start.elapsed().as_micros() as f64 / 1000_f64;
        terminal::done_msg(duration_ms);

        Ok(self)
    }

    // Execute database multiple statements (DDLs, admin query, etc..) using n_jobs threads.
    pub fn exec_stmts(
        &mut self,
        n_jobs: u32,
        stmts: Vec<String>,
        use_transaction: bool,
    ) -> Result<(), Error> {
        // We want to get one row per job and the statements balanced across the rows.
        let mut rows = Vec::with_capacity(n_jobs as usize);
        for _ in 0..n_jobs {
            rows.push(Vec::new());
        }
        for (n, stmt) in stmts.iter().enumerate() {
            rows[(n + 1) % n_jobs as usize].push(stmt.clone());
        }
        let mut jobs = Vec::new();

        for j in 1..=n_jobs {
            // Cloning values before passing them to the thread
            let job_stmts = rows[(j - 1) as usize].clone();
            let dsn = self.dsn.clone();
            let tls = self.tls.clone();

            // Starting a new job into its dedicated thread
            let job = thread::spawn(move || -> Result<(), Error> {
                // New database connection
                let mut client = Client::connect(&dsn, tls)?;

                for stmt in job_stmts.iter() {
                    if use_transaction {
                        let mut transaction = client.transaction()?;
                        transaction.batch_execute(stmt)?;
                        transaction.commit()?;
                    }
                    // No transaction
                    else {
                        client.batch_execute(stmt)?;
                    }
                }
                Ok(())
            });

            jobs.push(job);
        }

        // Wait for the end of all jobs, keeping the first failure.
        join_jobs(jobs)
    }

    // Execute primary keys DDLs using multiple concurrent jobs
    pub fn add_primary_keys(&mut self, n_jobs: u32) -> Result<&mut Self, Error> {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Primary keys creation");
        self.exec_stmts(n_jobs, benchmark.get_pkey_ddls(), true)?;
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        Ok(self)
    }

    // Execute foreign keys DDLs serially.
    //
    // FK creation is intentionally not parallelized: ALTER TABLE ... ADD FOREIGN KEY
    // takes a ShareRowExclusiveLock on both the referencing and the referenced table,
    // so concurrent adds touching a shared parent serialize on its lock anyway and can
    // deadlock when two FKs reference each other's tables in opposite order across jobs.
    pub fn add_foreign_keys(&mut self, _n_jobs: u32, no_fkey: bool) -> Result<&mut Self, Error> {
        // Don't do anything if no_fkey is true
        if no_fkey {
            return Ok(self);
        }

        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Foreign keys creation");
        self.exec_stmts(1, benchmark.get_fkey_ddls(), true)?;
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        Ok(self)
    }

    // Execute additional indexes DDLs using multiple concurrent jobs
    pub fn add_indexes(&mut self, n_jobs: u32) -> Result<&mut Self, Error> {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Additional indexes creation");
        self.exec_stmts(n_jobs, benchmark.get_index_ddls(), true)?;
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        Ok(self)
    }

    // Execute VACUUM statements using multiple concurrent jobs
    pub fn vacuum(&mut self, n_jobs: u32) -> Result<&mut Self, Error> {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Vacuuming tables");
        self.exec_stmts(n_jobs, benchmark.get_vacuum_stmts(), false)?;
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        Ok(self)
    }

    // Force a checkpoint
    pub fn checkpoint(&mut self) -> Result<&mut Self, Error> {
        let start = Instant::now();

        terminal::start_msg("INIT", "Forcing checkpoint");
        let mut client = self.connect()?;
        client.batch_execute("CHECKPOINT")?;

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        Ok(self)
    }
}

/// Join worker threads that return a Result, returning the first error after
/// every thread has finished (so no connection is left mid-statement).
fn join_jobs(jobs: Vec<JoinHandle<Result<(), Error>>>) -> Result<(), Error> {
    let mut first_error = None;
    for job in jobs {
        if let Err(error) = job.join().expect("the client thread panicked") {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
