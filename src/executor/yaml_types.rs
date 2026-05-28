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
