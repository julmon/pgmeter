// Canonical benchmark specs are baked into the binary from `samples/` at
// compile time, so `spec new` can scaffold an editable copy on disk without the
// files being present at runtime. include_dir tracks the tree, so adding a new
// sample folder embeds it on the next build with no manifest to maintain.

use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};
use serde::Deserialize;
use tabled::{
    builder::Builder,
    settings::{object::Columns, style::HorizontalLine, Modify, Style, Width},
};

static SAMPLES: Dir = include_dir!("$CARGO_MANIFEST_DIR/samples");

// Just the headline fields of a spec; every other field is ignored so this
// stays decoupled from the full BenchmarkSpec type.
#[derive(Deserialize)]
struct SpecMeta {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

// An embedded benchmark: the folder name (what you pass to `spec new`) plus the
// spec's headline `name`/`description`.
pub struct SpecInfo {
    pub dir: String,
    pub name: String,
    pub description: Option<String>,
}

// Folder names of the embedded benchmarks (the top-level dirs under `samples/`).
pub fn list() -> Vec<String> {
    let mut names: Vec<String> = SAMPLES
        .dirs()
        .filter_map(|d| {
            d.path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
        })
        .collect();
    names.sort();
    names
}

// Embedded benchmarks with their spec title/description, sorted by folder name.
pub fn list_entries() -> Vec<SpecInfo> {
    let mut infos: Vec<SpecInfo> = list()
        .into_iter()
        .map(|dir| {
            let meta = SAMPLES
                .get_file(format!("{}/spec.yaml", dir))
                .and_then(|f| f.contents_utf8())
                .and_then(|s| serde_yaml::from_str::<SpecMeta>(s).ok());
            let (name, description) = match meta {
                Some(m) => (m.name, m.description.map(|d| collapse_ws(&d))),
                None => (dir.clone(), None),
            };
            SpecInfo {
                dir,
                name,
                description,
            }
        })
        .collect();
    infos.sort_by(|a, b| a.dir.cmp(&b.dir));
    infos
}

// Collapse runs of whitespace (incl. the newlines a folded YAML `>` scalar
// leaves behind) into single spaces and trim, so a description renders on one
// tidy line.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Print the embedded benchmarks as a light, frame-less table (header rule only)
// matching the run-result tables. The Description column is wrapped so a long
// blurb doesn't blow the table out to the full terminal width.
pub fn print_list() {
    let mut builder = Builder::default();
    builder.push_record(["Benchmark", "Title", "Description"]);
    for e in list_entries() {
        builder.push_record([e.dir, e.name, e.description.unwrap_or_default()]);
    }

    let mut table = builder.build();
    table
        .with(Style::blank().horizontals([(1, HorizontalLine::new('─').intersection('─'))]))
        .with(Modify::new(Columns::new(2..=2)).with(Width::wrap(60).keep_words(true)));

    println!("{}", table);
}

// Scaffold `<output>/<benchmark>/` (spec.yaml + any scripts/) from the embedded
// copy. Refuses to overwrite an existing destination unless `force`.
pub fn new(benchmark: &str, output: &str, force: bool) -> Result<PathBuf, String> {
    let dir = SAMPLES.get_dir(benchmark).ok_or_else(|| {
        format!(
            "unknown benchmark '{}'. Available: {}",
            benchmark,
            list().join(", ")
        )
    })?;

    let dest_root = Path::new(output).join(benchmark);
    if dest_root.exists() && !force {
        return Err(format!(
            "'{}' already exists (use --force to overwrite)",
            dest_root.display()
        ));
    }

    write_dir(dir, Path::new(output))?;
    Ok(dest_root)
}

// Recursively write an embedded directory's files to disk. Embedded file paths
// are relative to the `samples/` root (e.g. "tpc-c/scripts/x.lua"), so joining
// them onto `output` reproduces the folder structure.
fn write_dir(dir: &Dir, output: &Path) -> Result<(), String> {
    for file in dir.files() {
        let dest = output.join(file.path());
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {}", parent.display(), e))?;
        }
        std::fs::write(&dest, file.contents())
            .map_err(|e| format!("write {}: {}", dest.display(), e))?;
    }
    for sub in dir.dirs() {
        write_dir(sub, output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn list_contains_every_bundled_benchmark_sorted() {
        let names = list();
        assert_eq!(
            names,
            [
                "custom-simple",
                "pgm-b",
                "pgm-c",
                "pgm-c-client",
                "pgm-ds",
                "pgm-h",
                "pgm-iot"
            ]
        );
        let entries = list_entries();
        assert_eq!(entries.len(), names.len());
        let pgm_c = entries.iter().find(|e| e.dir == "pgm-c").unwrap();
        assert_eq!(pgm_c.name, "PGM-C");
        assert!(pgm_c.description.as_deref().unwrap_or("").contains("TPC-C"));
    }

    #[test]
    fn new_scaffolds_spec_and_scripts_and_refuses_to_overwrite() {
        let tmp = temp_dir();
        let out = tmp.path().to_path_buf();
        let out_str = out.to_str().unwrap();

        let dest = new("pgm-c", out_str, false).unwrap();
        assert_eq!(dest, out.join("pgm-c"));
        assert!(dest.join("spec.yaml").is_file());
        assert!(dest.join("scripts").join("pgmc_load.lua").is_file());
        // The copy is byte-identical to the embedded sample.
        let embedded = SAMPLES.get_file("pgm-c/spec.yaml").unwrap().contents();
        assert_eq!(std::fs::read(dest.join("spec.yaml")).unwrap(), embedded);

        // Existing destination: refused without --force, replaced with it.
        std::fs::write(dest.join("spec.yaml"), "edited").unwrap();
        let err = new("pgm-c", out_str, false).unwrap_err();
        assert!(
            err.contains("already exists") && err.contains("--force"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("spec.yaml")).unwrap(),
            "edited"
        );
        new("pgm-c", out_str, true).unwrap();
        assert_eq!(std::fs::read(dest.join("spec.yaml")).unwrap(), embedded);

        // A spec-only sample has no scripts/ directory.
        let simple = new("custom-simple", out_str, false).unwrap();
        assert!(simple.join("spec.yaml").is_file());
        assert!(!simple.join("scripts").exists());
    }

    #[test]
    fn new_rejects_unknown_benchmark_and_lists_the_available_ones() {
        let tmp = temp_dir();
        let out = tmp.path().to_path_buf();
        let err = new("pgm-x", out.to_str().unwrap(), false).unwrap_err();
        assert!(err.contains("unknown benchmark 'pgm-x'"), "{err}");
        assert!(
            err.contains("custom-simple, pgm-b, pgm-c, pgm-c-client, pgm-ds, pgm-h, pgm-iot"),
            "{err}"
        );
        assert!(!out.join("pgm-x").exists());
    }
}
