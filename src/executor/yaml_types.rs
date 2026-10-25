use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub scaling: ScalingSpec,
    pub schema: SchemaSpec,
    #[serde(default)]
    pub load: Vec<LoadSpec>,
    pub transactions: Vec<TransactionSpec>,
    /// Headline throughput metric (unit label + time base). Defaults to TPM.
    #[serde(default)]
    pub metrics: MetricsSpec,
    /// Shared Lua modules to pre-load into every thread's VM
    #[serde(default)]
    pub scripts: ScriptsSpec,
    /// Lua script executed once before per-id loading (e.g. populate shared reference tables)
    #[serde(default)]
    pub pre_load_script: Option<String>,
    /// Lua script executed per-id for data loading (replaces template-based `load:` section)
    #[serde(default)]
    pub load_script: Option<String>,
}

/// Defines the report's headline throughput metric. `unit` is the label shown
/// on the KPI card, chart axis and table column; `per` is the time base used to
/// turn a committed-transaction count into a rate.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct MetricsSpec {
    /// e.g. "TPS", "txns/min", "queries/hr". Used purely as a display label.
    #[serde(default = "default_throughput_unit")]
    pub throughput_unit: String,
    /// Time base for the rate: "second", "minute" or "hour".
    #[serde(default = "default_throughput_per")]
    pub per: ThroughputPer,
}

impl Default for MetricsSpec {
    fn default() -> Self {
        MetricsSpec {
            throughput_unit: default_throughput_unit(),
            per: default_throughput_per(),
        }
    }
}

fn default_throughput_unit() -> String {
    "TPM".to_string()
}

fn default_throughput_per() -> ThroughputPer {
    ThroughputPer::Minute
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThroughputPer {
    Second,
    Minute,
    Hour,
}

impl ThroughputPer {
    /// Multiplier applied to a per-second transaction count to express the rate
    /// in this time base.
    pub fn factor(self) -> f64 {
        match self {
            ThroughputPer::Second => 1.0,
            ThroughputPer::Minute => 60.0,
            ThroughputPer::Hour => 3600.0,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScalingSpec {
    pub unit: String,
    pub max_id_query: String,
    #[serde(default)]
    pub constants: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SchemaSpec {
    pub tables: Vec<TableSpec>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeySpec>,
    #[serde(default)]
    pub indexes: Vec<IndexSpec>,
    /// Additional DDL statements executed before table creation (e.g. CREATE SEQUENCE)
    #[serde(default)]
    pub pre_ddls: Vec<String>,
    /// Additional DDL/DML statements appended to the vacuum step (e.g. setval for sequences)
    #[serde(default)]
    pub post_ddls: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TableSpec {
    pub name: String,
    pub columns: Vec<ColumnSpec>,
    #[serde(default)]
    pub primary_key: Vec<String>,
    /// Table access method (e.g. "heap", "columnar")
    /// Maps to: CREATE TABLE ... USING <access_method>
    #[serde(default)]
    pub access_method: Option<String>,
    /// Table storage options as key=value pairs
    /// Maps to: CREATE TABLE ... WITH (key=value, ...)
    #[serde(default)]
    pub storage: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ColumnSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub col_type: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ForeignKeySpec {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub references: ForeignKeyRef,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ForeignKeyRef {
    pub table: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct IndexSpec {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub unique: bool,
    /// Index access method (e.g. "btree", "hash", "gin", "gist", "brin").
    /// Maps to: CREATE INDEX ... ON <table> USING <type> (...)
    #[serde(rename = "type", default)]
    pub index_type: Option<String>,
    /// Non-key columns to add to a covering index.
    /// Maps to: CREATE INDEX ... (...) INCLUDE (<include>)
    #[serde(default)]
    pub include: Vec<String>,
    /// Predicate for a partial index.
    /// Maps to: CREATE INDEX ... (...) WHERE <where>
    #[serde(rename = "where", default)]
    pub predicate: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LoadSpec {
    pub table: String,
    /// How rows are loaded. Only `copy` exists; the field is an enum so a typo
    /// or an unsupported method is rejected when the spec is parsed.
    #[serde(default)]
    pub method: LoadMethod,
    pub copy_columns: Vec<String>,
    pub rows_per_id: serde_yaml::Value,
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
    pub row: String,
}

/// Data loading method for a `load:` entry.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LoadMethod {
    /// `COPY ... FROM STDIN`, the only implemented method.
    #[default]
    Copy,
}

fn default_batch_size() -> u32 {
    10_000
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TransactionSpec {
    pub id: u16,
    pub name: String,
    pub weight: u16,
    #[serde(default)]
    pub description: String,
    /// How the transaction is executed; rejected at parse time if unknown.
    #[serde(default)]
    pub mode: TransactionMode,
    /// Binding expressions, kept in declaration order so that later bindings
    /// may reference earlier ones. serde_yaml::Mapping preserves insertion order.
    #[serde(default)]
    pub bindings: serde_yaml::Mapping,
    pub sql: Option<String>,
    pub script: Option<String>,
    pub script_file: Option<String>,
}

/// Execution mode of a transaction.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransactionMode {
    /// `sql` template with `bindings` substituted, run with the simple protocol.
    #[default]
    Simple,
    /// Lua `script` / `script_file` driving the database bridge.
    Script,
}

/// Optional list of Lua files to pre-load into every thread's Lua VM.
/// These run at VM init time (before any transaction), useful for shared
/// helper functions and dispatch tables.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ScriptsSpec {
    #[serde(default)]
    pub modules: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_factor_per_time_base() {
        assert_eq!(ThroughputPer::Second.factor(), 1.0);
        assert_eq!(ThroughputPer::Minute.factor(), 60.0);
        assert_eq!(ThroughputPer::Hour.factor(), 3600.0);
    }

    #[test]
    fn metrics_default_is_tpm_per_minute() {
        let m = MetricsSpec::default();
        assert_eq!(m.throughput_unit, "TPM");
        assert_eq!(m.per, ThroughputPer::Minute);
    }

    #[test]
    fn metrics_deserializes_lowercase_per() {
        let m: MetricsSpec =
            serde_yaml::from_str("throughput_unit: queries/hr\nper: hour\n").unwrap();
        assert_eq!(m.throughput_unit, "queries/hr");
        assert_eq!(m.per, ThroughputPer::Hour);
    }

    #[test]
    fn metrics_fills_missing_fields_with_defaults() {
        // Only the unit is given; `per` falls back to minute.
        let m: MetricsSpec = serde_yaml::from_str("throughput_unit: TPS\n").unwrap();
        assert_eq!(m.throughput_unit, "TPS");
        assert_eq!(m.per, ThroughputPer::Minute);
    }

    // Regression for RELEASE_REVIEW B7: `mode` and `method` were free strings.
    // An unknown mode failed on every execution (thousands of identical error
    // lines) and an unknown method was silently ignored.
    #[test]
    fn unknown_mode_and_method_are_rejected_at_parse_time() {
        let tx: Result<TransactionSpec, _> =
            serde_yaml::from_str("id: 1\nname: t\nweight: 1\nmode: extended\nsql: SELECT 1\n");
        let err = tx.unwrap_err().to_string();
        assert!(err.contains("unknown variant `extended`"), "{err}");
        assert!(
            err.contains("`simple`") && err.contains("`script`"),
            "{err}"
        );

        let load: Result<LoadSpec, _> = serde_yaml::from_str(
            "table: t\nmethod: insert\ncopy_columns: [id]\nrows_per_id: 1\nrow: x\n",
        );
        let err = load.unwrap_err().to_string();
        assert!(err.contains("unknown variant `insert`"), "{err}");

        let tx: TransactionSpec =
            serde_yaml::from_str("id: 1\nname: t\nweight: 1\nmode: script\nscript: x\n").unwrap();
        assert_eq!(tx.mode, TransactionMode::Script);
    }

    // Regression for RELEASE_REVIEW B8: a misspelt key used to be dropped
    // silently, so the benchmark ran with a different schema or workload
    // than written.
    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        let cases = [
            // top level
            ("name: x\nscaling: {unit: u, max_id_query: q}\nschema: {tables: []}\ntransactions: []\nload_scripts: []\n", "load_scripts"),
            // table: primary_keys instead of primary_key
            ("name: x\nscaling: {unit: u, max_id_query: q}\ntransactions: []\nschema:\n  tables:\n    - name: t\n      columns: [{name: id, type: int}]\n      primary_keys: [id]\n", "primary_keys"),
            // transaction: script_files instead of script_file
            ("name: x\nscaling: {unit: u, max_id_query: q}\nschema: {tables: []}\ntransactions:\n  - {id: 1, name: t, weight: 1, mode: script, script_files: a.lua}\n", "script_files"),
            // load entry
            ("name: x\nscaling: {unit: u, max_id_query: q}\nschema: {tables: []}\ntransactions: []\nload:\n  - {table: t, copy_columns: [id], rows_per_id: 1, row: r, batch: 10}\n", "batch"),
            // metrics
            ("name: x\nscaling: {unit: u, max_id_query: q}\nschema: {tables: []}\ntransactions: []\nmetrics: {unit: TPS}\n", "unit"),
            // index
            ("name: x\nscaling: {unit: u, max_id_query: q}\ntransactions: []\nschema:\n  tables: []\n  indexes:\n    - {name: i, table: t, columns: [a], method: hash}\n", "method"),
        ];
        for (yaml, key) in cases {
            let err = serde_yaml::from_str::<BenchmarkSpec>(yaml)
                .err()
                .unwrap_or_else(|| panic!("unknown key `{key}` was accepted"))
                .to_string();
            assert!(
                err.contains(&format!("unknown field `{key}`")),
                "{key}: {err}"
            );
        }
    }

    #[test]
    fn spec_applies_field_defaults() {
        let yaml = r#"
name: demo
scaling:
  unit: thing
  max_id_query: "SELECT 1"
schema:
  tables:
    - name: t
      columns:
        - { name: id, type: int }
load:
  - table: t
    copy_columns: [id]
    rows_per_id: 1
    row: "{{id}}\n"
transactions:
  - id: 1
    name: only
    weight: 100
"#;
        let spec: BenchmarkSpec = serde_yaml::from_str(yaml).unwrap();
        // metrics block omitted -> default TPM/minute.
        assert_eq!(spec.metrics.throughput_unit, "TPM");
        assert_eq!(spec.metrics.per, ThroughputPer::Minute);
        // load method defaults to copy, batch_size to 10_000.
        assert_eq!(spec.load[0].method, LoadMethod::Copy);
        assert_eq!(spec.load[0].batch_size, 10_000);
        // transaction mode defaults to simple.
        assert_eq!(spec.transactions[0].mode, TransactionMode::Simple);
        // column type rename ("type" -> col_type) works.
        assert_eq!(spec.schema.tables[0].columns[0].col_type, "int");
    }
}
