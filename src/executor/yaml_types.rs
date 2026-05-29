use std::collections::HashMap;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
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
pub struct MetricsSpec {
    /// e.g. "TPS", "tpmC", "QphH". Used purely as a display label.
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
#[allow(dead_code)]
pub struct ScalingSpec {
    pub unit: String,
    pub max_id_query: String,
    #[serde(default)]
    pub constants: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
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
pub struct ColumnSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub col_type: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ForeignKeySpec {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub references: ForeignKeyRef,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ForeignKeyRef {
    pub table: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
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
#[allow(dead_code)]
pub struct LoadSpec {
    pub table: String,
    #[serde(default = "default_method")]
    pub method: String,
    pub copy_columns: Vec<String>,
    pub rows_per_id: serde_yaml::Value,
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
    pub row: String,
}

fn default_method() -> String {
    "copy".to_string()
}

fn default_batch_size() -> u32 {
    10_000
}

#[derive(Debug, Deserialize, Clone)]
pub struct TransactionSpec {
    pub id: u16,
    pub name: String,
    pub weight: u16,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Binding expressions, kept in declaration order so that later bindings
    /// may reference earlier ones. serde_yaml::Mapping preserves insertion order.
    #[serde(default)]
    pub bindings: serde_yaml::Mapping,
    pub sql: Option<String>,
    pub script: Option<String>,
    pub script_file: Option<String>,
}

fn default_mode() -> String {
    "simple".to_string()
}

/// Optional list of Lua files to pre-load into every thread's Lua VM.
/// These run at VM init time (before any transaction), useful for shared
/// helper functions and dispatch tables.
#[derive(Debug, Deserialize, Clone, Default)]
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
            serde_yaml::from_str("throughput_unit: QphH\nper: hour\n").unwrap();
        assert_eq!(m.throughput_unit, "QphH");
        assert_eq!(m.per, ThroughputPer::Hour);
    }

    #[test]
    fn metrics_fills_missing_fields_with_defaults() {
        // Only the unit is given; `per` falls back to minute.
        let m: MetricsSpec = serde_yaml::from_str("throughput_unit: TPS\n").unwrap();
        assert_eq!(m.throughput_unit, "TPS");
        assert_eq!(m.per, ThroughputPer::Minute);
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
        // load method defaults to "copy", batch_size to 10_000.
        assert_eq!(spec.load[0].method, "copy");
        assert_eq!(spec.load[0].batch_size, 10_000);
        // transaction mode defaults to "simple".
        assert_eq!(spec.transactions[0].mode, "simple");
        // column type rename ("type" -> col_type) works.
        assert_eq!(spec.schema.tables[0].columns[0].col_type, "int");
    }
}
