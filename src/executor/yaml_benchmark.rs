use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use postgres::Client;

use super::benchmark::{
    Benchmark, BenchmarkStmt, BenchmarkTransaction, PreparedStatements, ReadWriteSync,
};
use super::yaml_lua;
use super::yaml_types::{BenchmarkSpec, TransactionMode};

pub struct YamlBenchmark {
    pub spec: BenchmarkSpec,
    pub scalefactor: u32,
    pub min_id: u32,
    pub max_id: u32,
    pub table_ddls: Vec<BenchmarkStmt>,
    pub pkey_ddls: Vec<BenchmarkStmt>,
    pub fkey_ddls: Vec<BenchmarkStmt>,
    pub index_ddls: Vec<BenchmarkStmt>,
    pub vacuum_stmts: Vec<BenchmarkStmt>,
    pub transactions_rw: Vec<BenchmarkTransaction>,
    /// Lua scripts keyed by transaction id
    pub lua_scripts: HashMap<u16, String>,
    /// Shared Lua modules to pre-load into every thread's VM
    pub lua_modules: Vec<String>,
    /// Lua script for pre-load operations (runs once before per-id loading)
    pub pre_load_script: Option<String>,
    /// Lua script for per-id data loading (replaces template-based load section)
    pub load_script_src: Option<String>,
    /// Base directory for resolving relative script_file paths
    #[allow(dead_code)]
    pub spec_dir: PathBuf,
}

impl YamlBenchmark {
    /// Load a benchmark specification from either a benchmark directory
    /// (containing `spec.yaml` and an optional `scripts/` folder) or a single
    /// spec file. Relative `script_file`/module/load-script paths resolve
    /// against the benchmark directory (the spec file's parent).
    pub fn from_file(path: &str, scalefactor: u32, min_id: u32, max_id: u32) -> YamlBenchmark {
        let path = Path::new(path);
        let (spec_path, spec_dir) = if path.is_dir() {
            (path.join("spec.yaml"), path.to_path_buf())
        } else {
            let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            (path.to_path_buf(), dir)
        };

        let yaml_content = std::fs::read_to_string(&spec_path).unwrap_or_else(|e| {
            super::terminal::err_msg(&format!(
                "Failed to read spec file '{}': {}",
                spec_path.display(),
                e
            ));
            std::process::exit(1);
        });

        let spec: BenchmarkSpec = serde_yaml::from_str(&yaml_content).unwrap_or_else(|e| {
            super::terminal::err_msg(&format!(
                "Failed to parse YAML spec '{}': {}",
                spec_path.display(),
                e
            ));
            std::process::exit(1);
        });

        Self::from_spec(spec, spec_dir, scalefactor, min_id, max_id)
    }

    /// Build the benchmark from an already parsed spec. Script and module
    /// files referenced by the spec are resolved relative to `spec_dir`.
    pub fn from_spec(
        spec: BenchmarkSpec,
        spec_dir: PathBuf,
        scalefactor: u32,
        min_id: u32,
        max_id: u32,
    ) -> YamlBenchmark {
        if let Err(problems) = validate_transactions(&spec) {
            super::terminal::err_msg(&format!(
                "Invalid benchmark spec '{}':\n  - {}",
                spec.name,
                problems.join("\n  - ")
            ));
            std::process::exit(1);
        }

        let table_ddls = Self::generate_table_ddls(&spec);
        let pkey_ddls = Self::generate_pkey_ddls(&spec);
        let fkey_ddls = Self::generate_fkey_ddls(&spec);
        let index_ddls = Self::generate_index_ddls(&spec);
        let vacuum_stmts = Self::generate_vacuum_stmts(&spec);
        let transactions_rw = Self::generate_transactions(&spec);
        let lua_scripts = Self::load_lua_scripts(&spec, &spec_dir);
        let lua_modules = Self::load_lua_modules(&spec, &spec_dir);
        let pre_load_script = Self::load_optional_script(&spec.pre_load_script, &spec_dir);
        let load_script_src = Self::load_optional_script(&spec.load_script, &spec_dir);

        YamlBenchmark {
            spec,
            scalefactor,
            min_id,
            max_id,
            table_ddls,
            pkey_ddls,
            fkey_ddls,
            index_ddls,
            vacuum_stmts,
            transactions_rw,
            lua_scripts,
            lua_modules,
            pre_load_script,
            load_script_src,
            spec_dir,
        }
    }

    fn generate_table_ddls(spec: &BenchmarkSpec) -> Vec<BenchmarkStmt> {
        let mut ddls = Vec::new();

        // Pre-DDLs (sequences, extensions, etc.)
        for sql in &spec.schema.pre_ddls {
            ddls.push(BenchmarkStmt { sql: sql.clone() });
        }

        for table in &spec.schema.tables {
            // Drop table if exists
            ddls.push(BenchmarkStmt {
                sql: format!("DROP TABLE IF EXISTS {} CASCADE", table.name),
            });

            // Build CREATE TABLE
            let columns: Vec<String> = table
                .columns
                .iter()
                .map(|c| format!("{} {}", c.name, c.col_type))
                .collect();

            let mut sql = format!("CREATE TABLE {} ({})", table.name, columns.join(", "));

            // USING <access_method>
            if let Some(ref am) = table.access_method {
                sql.push_str(&format!(" USING {}", am));
            }

            // WITH (key=value, ...)
            if !table.storage.is_empty() {
                let opts: Vec<String> = table
                    .storage
                    .iter()
                    .map(|(k, v)| match v {
                        serde_yaml::Value::Bool(b) => format!("{} = {}", k, b),
                        serde_yaml::Value::Number(n) => format!("{} = {}", k, n),
                        serde_yaml::Value::String(s) => format!("{} = {}", k, s),
                        _ => format!(
                            "{} = {}",
                            k,
                            serde_yaml::to_string(v).unwrap_or_default().trim()
                        ),
                    })
                    .collect();
                sql.push_str(&format!(" WITH ({})", opts.join(", ")));
            }

            ddls.push(BenchmarkStmt { sql });
        }

        ddls
    }

    fn generate_pkey_ddls(spec: &BenchmarkSpec) -> Vec<BenchmarkStmt> {
        let mut ddls = Vec::new();

        for table in &spec.schema.tables {
            if !table.primary_key.is_empty() {
                ddls.push(BenchmarkStmt {
                    sql: format!(
                        "ALTER TABLE {} ADD PRIMARY KEY ({})",
                        table.name,
                        table.primary_key.join(", ")
                    ),
                });
            }
        }

        ddls
    }

    fn generate_fkey_ddls(spec: &BenchmarkSpec) -> Vec<BenchmarkStmt> {
        spec.schema
            .foreign_keys
            .iter()
            .map(|fk| BenchmarkStmt {
                sql: format!(
                    "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
                    fk.table,
                    fk.name,
                    fk.columns.join(", "),
                    fk.references.table,
                    fk.references.columns.join(", ")
                ),
            })
            .collect()
    }

    fn generate_index_ddls(spec: &BenchmarkSpec) -> Vec<BenchmarkStmt> {
        spec.schema
            .indexes
            .iter()
            .map(|idx| {
                let unique = if idx.unique { "UNIQUE " } else { "" };
                let using = match &idx.index_type {
                    Some(t) => format!("USING {} ", t),
                    None => String::new(),
                };
                let include = if idx.include.is_empty() {
                    String::new()
                } else {
                    format!(" INCLUDE ({})", idx.include.join(", "))
                };
                let predicate = match &idx.predicate {
                    Some(p) => format!(" WHERE {}", p),
                    None => String::new(),
                };
                BenchmarkStmt {
                    sql: format!(
                        "CREATE {}INDEX {} ON {} {}({}){}{}",
                        unique,
                        idx.name,
                        idx.table,
                        using,
                        idx.columns.join(", "),
                        include,
                        predicate
                    ),
                }
            })
            .collect()
    }

    fn generate_vacuum_stmts(spec: &BenchmarkSpec) -> Vec<BenchmarkStmt> {
        let mut stmts: Vec<BenchmarkStmt> = spec
            .schema
            .tables
            .iter()
            .map(|t| BenchmarkStmt {
                sql: format!("VACUUM FREEZE ANALYZE {}", t.name),
            })
            .collect();

        // Post-DDLs (sequence resets, etc.)
        for sql in &spec.schema.post_ddls {
            stmts.push(BenchmarkStmt { sql: sql.clone() });
        }

        stmts
    }

    fn generate_transactions(spec: &BenchmarkSpec) -> Vec<BenchmarkTransaction> {
        spec.transactions
            .iter()
            .map(|tx| BenchmarkTransaction {
                id: tx.id,
                name: tx.name.clone(),
                weight: tx.weight,
                description: tx.description.clone(),
            })
            .collect()
    }

    fn load_optional_script(value: &Option<String>, spec_dir: &Path) -> Option<String> {
        value.as_ref().map(|v| {
            // If the value contains a newline, it's inline Lua code.
            // Otherwise, treat it as a file path.
            if v.contains('\n') {
                v.clone()
            } else {
                let full_path = spec_dir.join(v);
                std::fs::read_to_string(&full_path).unwrap_or_else(|e| {
                    super::terminal::err_msg(&format!(
                        "Failed to read script '{}': {}",
                        full_path.display(),
                        e
                    ));
                    std::process::exit(1);
                })
            }
        })
    }

    fn load_lua_modules(spec: &BenchmarkSpec, spec_dir: &Path) -> Vec<String> {
        spec.scripts
            .modules
            .iter()
            .map(|module| {
                // A value containing a newline is inline Lua source;
                // otherwise it is a file path relative to the spec.
                if module.contains('\n') {
                    module.clone()
                } else {
                    let path = spec_dir.join(module);
                    std::fs::read_to_string(&path).unwrap_or_else(|e| {
                        super::terminal::err_msg(&format!(
                            "Failed to read Lua module '{}': {}",
                            path.display(),
                            e
                        ));
                        std::process::exit(1);
                    })
                }
            })
            .collect()
    }

    fn load_lua_scripts(spec: &BenchmarkSpec, spec_dir: &Path) -> HashMap<u16, String> {
        let mut scripts = HashMap::new();

        for tx in &spec.transactions {
            // Inline script takes precedence
            if let Some(ref script) = tx.script {
                scripts.insert(tx.id, script.clone());
                continue;
            }

            // External script file
            if let Some(ref script_file) = tx.script_file {
                let script_path = spec_dir.join(script_file);
                let script = std::fs::read_to_string(&script_path).unwrap_or_else(|e| {
                    super::terminal::err_msg(&format!(
                        "Failed to read Lua script '{}': {}",
                        script_path.display(),
                        e
                    ));
                    std::process::exit(1);
                });
                scripts.insert(tx.id, script);
            }
        }

        scripts
    }

    /// Evaluate bindings and substitute {{var}} placeholders in a SQL template.
    /// Uses a thread-local Lua VM for binding evaluation (no per-call VM creation).
    fn render_simple_sql(
        &self,
        tx: &super::yaml_types::TransactionSpec,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let sql_template = match &tx.sql {
            Some(s) => s.as_str(),
            None => {
                return Err("Transaction has mode=simple but no sql field".into());
            }
        };

        // Collect bindings as ordered pairs. serde_yaml::Mapping preserves declaration
        // order, so later bindings may reference earlier ones.
        let bindings: Vec<(String, String)> = tx
            .bindings
            .iter()
            .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
            .collect();

        // Evaluate all bindings using the thread-local Lua VM
        let resolved = yaml_lua::eval_bindings_simple(
            tx.id,
            &bindings,
            self.min_id,
            self.max_id,
            self.scalefactor,
            &self.spec.scaling.constants,
        )?;

        // Substitute {{var}} in the SQL template
        let mut sql = sql_template.to_string();
        for (key, val) in &resolved {
            let placeholder = format!("{{{{{}}}}}", key);
            sql = sql.replace(&placeholder, val);
        }

        Ok(sql)
    }

    /// Execute a template-based COPY data load for a single load spec and set of IDs.
    /// Row generation is fully batched inside Lua: a single Lua call generates all rows
    /// for a batch as one concatenated string, minimizing Rust↔Lua boundary crossings.
    fn execute_load(
        &self,
        client: &mut Client,
        load_spec: &super::yaml_types::LoadSpec,
        ids: &[u32],
    ) -> Result<(), String> {
        let lua = yaml_lua::create_eval_vm_with_context(
            self.min_id,
            self.max_id,
            self.scalefactor,
            &self.spec.scaling.constants,
        )
        .map_err(|e| format!("Failed to create Lua VM for data loading: {}", e))?;

        // Resolve rows_per_id
        let rows_per_id: u32 = match &load_spec.rows_per_id {
            serde_yaml::Value::Number(n) => n.as_u64().unwrap_or(1) as u32,
            serde_yaml::Value::String(s) => {
                let code = format!("return ({})", s.replace("{{", "").replace("}}", ""));
                lua.load(&code)
                    .eval::<u32>()
                    .map_err(|e| format!("Failed to evaluate rows_per_id '{}': {}", s, e))?
            }
            _ => 1,
        };

        // Pre-compile a batch generator: a Lua function that takes (id, i_start, i_end)
        // and returns all rows for that range as a single concatenated string.
        // This reduces Rust↔Lua crossings from one per row to one per batch.
        let batch_func = yaml_lua::compile_batch_row_template(&lua, &load_spec.row)
            .map_err(|e| format!("Failed to compile row template: {}", e))?;

        let copy_sql = format!(
            "COPY {} ({}) FROM stdin",
            load_spec.table,
            load_spec.copy_columns.join(", ")
        );

        for &id in ids {
            let batch_size = load_spec.batch_size;
            let n_batches = ((rows_per_id as f64) / (batch_size as f64)).ceil() as u32;

            for batch in 0..n_batches {
                let batch_start = batch * batch_size + 1;
                let batch_end = std::cmp::min((batch + 1) * batch_size, rows_per_id);

                // Single Lua call generates all rows for this batch
                let data: String = batch_func.call((id, batch_start, batch_end)).map_err(|e| {
                    format!(
                        "Batch generation failed (id={}, {}-{}): {}",
                        id, batch_start, batch_end, e
                    )
                })?;

                let mut writer = client
                    .copy_in(&copy_sql)
                    .map_err(|e| format!("COPY failed: {}", e))?;

                writer
                    .write_all(data.as_bytes())
                    .map_err(|e| format!("Write failed: {}", e))?;

                writer
                    .finish()
                    .map_err(|e| format!("COPY finish failed: {}", e))?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

/// Check that every transaction carries what its mode needs: `simple` needs
/// `sql`, `script` needs exactly one of `script` / `script_file`. Returns every
/// problem found so a spec author fixes them in one pass.
fn validate_transactions(spec: &BenchmarkSpec) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    for tx in &spec.transactions {
        let who = format!("transaction {} ({})", tx.id, tx.name);
        match tx.mode {
            TransactionMode::Simple => {
                if tx.sql.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    problems.push(format!("{who}: mode `simple` requires a non-empty `sql`"));
                }
            }
            TransactionMode::Script => match (&tx.script, &tx.script_file) {
                (None, None) => problems.push(format!(
                    "{who}: mode `script` requires `script` or `script_file`"
                )),
                (Some(_), Some(_)) => problems.push(format!(
                    "{who}: `script` and `script_file` are mutually exclusive"
                )),
                (Some(s), None) if s.trim().is_empty() => {
                    problems.push(format!("{who}: `script` is empty"))
                }
                _ => {}
            },
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// A failed statement leaves the connection inside an aborted transaction
/// block (a `simple` template stops at the first error before its COMMIT; a
/// script errors after `begin()`), and every later statement on that
/// connection then fails with "current transaction is aborted". Issue a
/// best-effort ROLLBACK so the next transaction starts clean. Outside a
/// transaction block PostgreSQL only emits a warning, so this is always safe;
/// a failure here means the connection itself is gone, which the next
/// transaction reports on its own.
fn recover_connection(client: &mut Client) {
    let _ = client.batch_execute("ROLLBACK");
}

/// First column of the `max_id_query` result as an integer, whatever the
/// integer width PostgreSQL chose (`COUNT(*)` is bigint, `MAX(int)` is int).
fn max_id_column_value(row: &postgres::Row) -> Option<Option<i64>> {
    if let Ok(v) = row.try_get::<_, Option<i64>>(0) {
        return Some(v);
    }
    if let Ok(v) = row.try_get::<_, Option<i32>>(0) {
        return Some(v.map(i64::from));
    }
    if let Ok(v) = row.try_get::<_, Option<i16>>(0) {
        return Some(v.map(i64::from));
    }
    None
}

/// Validate the value returned by `max_id_query`: ids are 1-based `u32`s.
fn max_id_from_value(value: Option<i64>) -> Result<u32, String> {
    match value {
        None => Err("max_id_query returned NULL: no data found, run `pgmeter init` first".into()),
        Some(v) if v < 1 => Err(format!(
            "max_id_query returned {v}: no data found, run `pgmeter init` first"
        )),
        Some(v) => u32::try_from(v).map_err(|_| {
            format!(
                "max_id_query returned {v}, which exceeds the maximum id {}",
                u32::MAX
            )
        }),
    }
}

impl ReadWriteSync for YamlBenchmark {
    fn prepare_statements(
        &self,
        _client: &mut Client,
    ) -> Result<PreparedStatements, postgres::Error> {
        Ok(PreparedStatements::new())
    }

    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        _prepared: &PreparedStatements,
        transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        // Find the transaction spec
        let tx_spec = self
            .spec
            .transactions
            .iter()
            .find(|t| t.id == transaction.id)
            .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                format!("Transaction id {} not found in spec", transaction.id).into()
            })?;

        let result = match tx_spec.mode {
            // Evaluate bindings, substitute into the SQL template, execute.
            // No `?` here: a failure must fall through to the recovery below.
            TransactionMode::Simple => self.render_simple_sql(tx_spec).and_then(|sql| {
                let start = Instant::now();
                client
                    .batch_execute(&sql)
                    .map(|_| start.elapsed().as_micros())
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
            }),
            TransactionMode::Script => {
                // Execute Lua script. Validation at load time guarantees one
                // exists; never fall back to an empty chunk, which would run,
                // "commit" nothing and inflate throughput.
                let Some(script) = self.lua_scripts.get(&transaction.id) else {
                    return Err(format!(
                        "transaction {} ({}) is in script mode but has no script",
                        transaction.id, transaction.name
                    )
                    .into());
                };

                yaml_lua::execute_script(
                    client,
                    script,
                    self.min_id,
                    self.max_id,
                    self.scalefactor,
                    &self.spec.scaling.constants,
                    &self.lua_scripts,
                    &self.lua_modules,
                    transaction.id,
                )
            }
        };

        if result.is_err() {
            recover_connection(client);
        }
        result
    }
}

impl Benchmark for YamlBenchmark {
    fn initialize_schema(&self, client: &mut Client) -> Result<u128, postgres::Error> {
        let start = Instant::now();

        let mut transaction = client.transaction()?;
        for ddl in &self.table_ddls {
            transaction.batch_execute(&ddl.sql)?;
        }
        transaction.commit()?;

        Ok(start.elapsed().as_micros())
    }

    fn pre_load_data(&self, client: &mut Client) -> Result<u128, String> {
        match &self.pre_load_script {
            Some(script) => yaml_lua::execute_load_script(
                client,
                script,
                self.scalefactor,
                &self.spec.scaling.constants,
                &[],
            ),
            None => Ok(0),
        }
    }

    fn load_data(&self, client: &mut Client, ids: Vec<u32>) -> Result<u128, String> {
        let start = Instant::now();

        // If a load_script is defined, use it (Lua-based loading)
        if let Some(ref script) = self.load_script_src {
            yaml_lua::execute_load_script(
                client,
                script,
                self.scalefactor,
                &self.spec.scaling.constants,
                &ids,
            )?;
        } else {
            // Otherwise use template-based COPY loading
            for load_spec in &self.spec.load {
                self.execute_load(client, load_spec, &ids)?;
            }
        }

        Ok(start.elapsed().as_micros())
    }

    /// Run the spec's `max_id_query` and turn its first column into the upper
    /// bound of the id range. Accepts smallint, integer and bigint; NULL
    /// (`MAX()` over an empty table), no rows, or a value below 1 mean the
    /// dataset has not been loaded and are reported as errors, not panics.
    fn get_default_max_id(
        &self,
        client: &mut Client,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        let query = &self.spec.scaling.max_id_query;
        let rows = client.query(query, &[])?;
        let Some(row) = rows.first() else {
            return Err(format!("max_id_query returned no rows: {query}").into());
        };
        if row.is_empty() {
            return Err(format!("max_id_query returned no columns: {query}").into());
        }
        let value = max_id_column_value(row)
            .ok_or_else(|| {
                format!(
                    "max_id_query must return an integer (smallint, integer or bigint), got {}: {query}",
                    row.columns()[0].type_()
                )
            })?;
        max_id_from_value(value).map_err(|e| format!("{e}: {query}").into())
    }

    fn get_transactions_rw(&self) -> Vec<BenchmarkTransaction> {
        self.transactions_rw.clone()
    }

    fn get_pkey_ddls(&self) -> Vec<BenchmarkStmt> {
        self.pkey_ddls.clone()
    }

    fn get_fkey_ddls(&self) -> Vec<BenchmarkStmt> {
        self.fkey_ddls.clone()
    }

    fn get_index_ddls(&self) -> Vec<BenchmarkStmt> {
        self.index_ddls.clone()
    }

    fn get_vacuum_stmts(&self) -> Vec<BenchmarkStmt> {
        self.vacuum_stmts.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::YamlBenchmark;
    use crate::executor::yaml_types::BenchmarkSpec;

    fn spec_with_transactions(transactions_yaml: &str) -> BenchmarkSpec {
        let yaml = format!(
            "name: v\nscaling:\n  unit: u\n  max_id_query: \"SELECT 1\"\nschema:\n  tables: []\ntransactions:\n{transactions_yaml}"
        );
        serde_yaml::from_str(&yaml).unwrap()
    }

    // Regression for RELEASE_REVIEW B6: a `mode: script` transaction with no
    // script used to load fine and run an empty chunk that counted as a commit.
    #[test]
    fn validate_rejects_script_mode_without_a_script() {
        let spec =
            spec_with_transactions("  - {id: 1, name: no-script, weight: 50, mode: script}\n");
        let problems = super::validate_transactions(&spec).unwrap_err();
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].contains("transaction 1 (no-script)"),
            "{problems:?}"
        );
        assert!(
            problems[0].contains("requires `script` or `script_file`"),
            "{problems:?}"
        );
    }

    #[test]
    fn validate_reports_every_problem_and_accepts_valid_specs() {
        let spec = spec_with_transactions(
            "  - {id: 1, name: both, weight: 10, mode: script, script: 'exec(\"SELECT 1\")', script_file: x.lua}\n\
             \x20 - {id: 2, name: empty-script, weight: 10, mode: script, script: '  '}\n\
             \x20 - {id: 3, name: no-sql, weight: 10, mode: simple}\n\
             \x20 - {id: 5, name: ok-simple, weight: 10, sql: 'SELECT 1'}\n\
             \x20 - {id: 6, name: ok-script, weight: 10, mode: script, script_file: tx.lua}\n",
        );
        let problems = super::validate_transactions(&spec).unwrap_err();
        let joined = problems.join("\n");
        assert_eq!(problems.len(), 3, "{joined}");
        assert!(joined.contains("(both): `script` and `script_file` are mutually exclusive"));
        assert!(joined.contains("(empty-script): `script` is empty"));
        assert!(joined.contains("(no-sql): mode `simple` requires a non-empty `sql`"));

        let valid = spec_with_transactions(
            "  - {id: 5, name: ok-simple, weight: 10, sql: 'SELECT 1'}\n\
             \x20 - {id: 6, name: ok-script, weight: 10, mode: script, script: 'exec(\"SELECT 1\")'}\n",
        );
        assert!(super::validate_transactions(&valid).is_ok());
    }

    // Every bundled sample must pass the same validation users' specs get.
    #[test]
    fn bundled_samples_pass_transaction_validation() {
        let samples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let mut checked = 0;
        for entry in std::fs::read_dir(&samples).unwrap() {
            let spec_path = entry.unwrap().path().join("spec.yaml");
            if !spec_path.is_file() {
                continue;
            }
            let yaml = std::fs::read_to_string(&spec_path).unwrap();
            let spec: BenchmarkSpec = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("{}: {e}", spec_path.display()));
            if let Err(problems) = super::validate_transactions(&spec) {
                panic!("{}: {}", spec_path.display(), problems.join("; "));
            }
            checked += 1;
        }
        assert!(
            checked >= 6,
            "expected the six bundled samples, found {checked}"
        );
    }

    #[test]
    fn max_id_value_rejects_null_zero_negative_and_overflow() {
        // Regression for RELEASE_REVIEW B4: these used to panic inside
        // `Row::get` ("a Postgres value was NULL") or be accepted as 0.
        assert!(super::max_id_from_value(None)
            .unwrap_err()
            .contains("run `pgmeter init` first"));
        assert!(super::max_id_from_value(Some(0)).is_err());
        assert!(super::max_id_from_value(Some(-3)).is_err());
        assert!(super::max_id_from_value(Some(i64::from(u32::MAX) + 1)).is_err());
        assert_eq!(super::max_id_from_value(Some(1)), Ok(1));
        assert_eq!(
            super::max_id_from_value(Some(i64::from(u32::MAX))),
            Ok(u32::MAX)
        );
    }

    // Regression for RELEASE_REVIEW B4 against a live server (skipped unless
    // PGMETER_TEST_DSN is set): NULL from MAX() over an empty table, bigint
    // from COUNT(*), smallint, and a non-integer column.
    #[test]
    fn max_id_query_handles_null_and_integer_widths() {
        use crate::executor::benchmark::Benchmark;
        use postgres::{Client, NoTls};

        let Ok(dsn) = std::env::var("PGMETER_TEST_DSN") else {
            eprintln!("PGMETER_TEST_DSN not set, skipping");
            return;
        };
        let mut client = Client::connect(&dsn, NoTls).expect("connect to PGMETER_TEST_DSN");

        let bench = |query: &str| {
            let mut spec = sample_spec();
            spec.scaling.max_id_query = query.to_string();
            YamlBenchmark::from_spec(spec, std::env::temp_dir(), 1, 1, 1)
        };

        // run before init: MAX over nothing is NULL -> clean error, no panic.
        let err = bench("SELECT MAX(x) FROM (VALUES (1)) v(x) WHERE false")
            .get_default_max_id(&mut client)
            .unwrap_err();
        assert!(
            err.to_string().contains("run `pgmeter init` first"),
            "{err}"
        );

        // Zero rows and zero value are errors too.
        assert!(bench("SELECT 1 WHERE false")
            .get_default_max_id(&mut client)
            .is_err());
        assert!(bench("SELECT 0").get_default_max_id(&mut client).is_err());

        // Every integer width is accepted.
        assert_eq!(
            bench("SELECT 7::smallint")
                .get_default_max_id(&mut client)
                .unwrap(),
            7
        );
        assert_eq!(
            bench("SELECT 7::int")
                .get_default_max_id(&mut client)
                .unwrap(),
            7
        );
        assert_eq!(
            bench("SELECT COUNT(*) FROM (VALUES (1), (2)) v")
                .get_default_max_id(&mut client)
                .unwrap(),
            2
        );

        // A non-integer column is reported, not panicked on.
        let err = bench("SELECT 'abc'")
            .get_default_max_id(&mut client)
            .unwrap_err();
        assert!(err.to_string().contains("must return an integer"), "{err}");
    }

    // Regression for RELEASE_REVIEW B3. Needs a live server: set
    // PGMETER_TEST_DSN (CI's e2e job does); skipped otherwise.
    //
    // A failing statement leaves the connection in an aborted transaction
    // block. Before the fix, every statement after the failure on that
    // connection errored with "current transaction is aborted, commands
    // ignored until end of transaction block", in both transaction modes.
    #[test]
    fn failed_transaction_leaves_connection_usable() {
        use crate::executor::benchmark::{Benchmark, ReadWriteSync};
        use postgres::{Client, NoTls};

        let Ok(dsn) = std::env::var("PGMETER_TEST_DSN") else {
            eprintln!("PGMETER_TEST_DSN not set, skipping");
            return;
        };

        let dir = std::env::temp_dir().join(format!("pgmeter-b3-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spec_path = dir.join("spec.yaml");
        std::fs::write(
            &spec_path,
            r#"
name: b3
scaling:
  unit: thing
  max_id_query: "SELECT 1"
schema:
  tables: []
transactions:
  - id: 1
    name: simple-fails
    weight: 50
    mode: simple
    sql: |
      BEGIN;
      SELECT 1 / 0;
      COMMIT;
  - id: 2
    name: script-fails
    weight: 50
    mode: script
    script: |
      begin()
      exec("SELECT 1 / 0")
      commit()
"#,
        )
        .unwrap();

        let benchmark = YamlBenchmark::from_file(spec_path.to_str().unwrap(), 1, 1, 1);
        let mut client = Client::connect(&dsn, NoTls).expect("connect to PGMETER_TEST_DSN");
        let prepared = benchmark.prepare_statements(&mut client).unwrap();

        for tx in benchmark.get_transactions_rw() {
            let err = benchmark
                .execute_rw_transaction_sync(&mut client, &prepared, &tx)
                .expect_err("division by zero must fail");
            // The server message must survive in both modes (RELEASE_REVIEW
            // B16): directly in the Lua runtime error for script mode, and in
            // the postgres::Error source chain for simple mode.
            let mut chain = err.to_string();
            let mut source = err.source();
            while let Some(cause) = source {
                chain.push_str(": ");
                chain.push_str(&cause.to_string());
                source = cause.source();
            }
            assert!(
                chain.contains("division by zero"),
                "{}: server message lost, got: {chain}",
                tx.name
            );
            // The connection must be out of the aborted block again.
            let row = client
                .query_one("SELECT 42", &[])
                .unwrap_or_else(|e| panic!("{}: connection unusable after failure: {e}", tx.name));
            assert_eq!(row.get::<_, i32>(0), 42);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    fn sample_spec() -> BenchmarkSpec {
        let yaml = r#"
name: full
scaling:
  unit: warehouse
  max_id_query: "SELECT max(id) FROM t1"
schema:
  pre_ddls:
    - "CREATE SEQUENCE s1"
  tables:
    - name: t1
      columns:
        - { name: id, type: "int" }
        - { name: val, type: "text" }
      primary_key: [id]
    - name: t2
      access_method: heap
      storage:
        fillfactor: 80
      columns:
        - { name: id, type: "int" }
        - { name: t1_id, type: "int" }
  foreign_keys:
    - name: t2_t1_fk
      table: t2
      columns: [t1_id]
      references:
        table: t1
        columns: [id]
  indexes:
    - name: idx_t2_t1
      table: t2
      columns: [t1_id]
    - name: uidx_t1_val
      table: t1
      columns: [val]
      unique: true
      type: btree
      include: [id]
      where: "val IS NOT NULL"
  post_ddls:
    - "SELECT setval('s1', 100)"
transactions:
  - id: 1
    name: tx-one
    weight: 70
    description: first
    sql: "SELECT 1"
  - id: 2
    name: tx-two
    weight: 30
    sql: "SELECT 2"
"#;
        serde_yaml::from_str(yaml).expect("valid spec")
    }

    #[test]
    fn table_ddls_include_pre_ddls_drop_create_storage() {
        let spec = sample_spec();
        let ddls: Vec<String> = YamlBenchmark::generate_table_ddls(&spec)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            ddls,
            vec![
                "CREATE SEQUENCE s1".to_string(),
                "DROP TABLE IF EXISTS t1 CASCADE".to_string(),
                "CREATE TABLE t1 (id int, val text)".to_string(),
                "DROP TABLE IF EXISTS t2 CASCADE".to_string(),
                "CREATE TABLE t2 (id int, t1_id int) USING heap WITH (fillfactor = 80)".to_string(),
            ],
        );
    }

    #[test]
    fn pkey_ddls_only_for_tables_with_primary_key() {
        let spec = sample_spec();
        let ddls: Vec<String> = YamlBenchmark::generate_pkey_ddls(&spec)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        // Only t1 declares a primary key.
        assert_eq!(
            ddls,
            vec!["ALTER TABLE t1 ADD PRIMARY KEY (id)".to_string()]
        );
    }

    #[test]
    fn fkey_ddls_render_references() {
        let spec = sample_spec();
        let ddls: Vec<String> = YamlBenchmark::generate_fkey_ddls(&spec)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            ddls,
            vec![
                "ALTER TABLE t2 ADD CONSTRAINT t2_t1_fk FOREIGN KEY (t1_id) REFERENCES t1 (id)"
                    .to_string()
            ],
        );
    }

    #[test]
    fn index_ddls_render_all_clauses() {
        let spec = sample_spec();
        let ddls: Vec<String> = YamlBenchmark::generate_index_ddls(&spec)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            ddls,
            vec![
                "CREATE INDEX idx_t2_t1 ON t2 (t1_id)".to_string(),
                "CREATE UNIQUE INDEX uidx_t1_val ON t1 USING btree (val) INCLUDE (id) WHERE val IS NOT NULL".to_string(),
            ],
        );
    }

    #[test]
    fn vacuum_stmts_cover_tables_then_post_ddls() {
        let spec = sample_spec();
        let stmts: Vec<String> = YamlBenchmark::generate_vacuum_stmts(&spec)
            .into_iter()
            .map(|s| s.sql)
            .collect();
        assert_eq!(
            stmts,
            vec![
                "VACUUM FREEZE ANALYZE t1".to_string(),
                "VACUUM FREEZE ANALYZE t2".to_string(),
                "SELECT setval('s1', 100)".to_string(),
            ],
        );
    }

    #[test]
    fn transactions_preserve_id_name_weight() {
        let spec = sample_spec();
        let txs = YamlBenchmark::generate_transactions(&spec);
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0].id, 1);
        assert_eq!(txs[0].name, "tx-one");
        assert_eq!(txs[0].weight, 70);
        assert_eq!(txs[0].description, "first");
        assert_eq!(txs[1].id, 2);
        assert_eq!(txs[1].weight, 30);
    }

    #[test]
    fn shipped_sample_specs_parse_and_generate_ddls() {
        for name in ["pgm-b", "pgm-c", "pgm-h", "pgm-ds", "pgm-iot"] {
            let path = format!("samples/{}/spec.yaml", name);
            let content =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e));
            let spec: BenchmarkSpec =
                serde_yaml::from_str(&content).unwrap_or_else(|e| panic!("parse {}: {}", path, e));
            assert!(!spec.schema.tables.is_empty(), "{} has no tables", name);
            assert!(
                !spec.transactions.is_empty(),
                "{} has no transactions",
                name
            );
            assert!(
                !YamlBenchmark::generate_table_ddls(&spec).is_empty(),
                "{} produced no table DDLs",
                name,
            );
        }
    }
}
