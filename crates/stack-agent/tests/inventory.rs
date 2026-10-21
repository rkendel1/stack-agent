//! The crate inventory in `docs/product/crates.md` stays synchronised with the workspace.
//!
//! Small and deterministic on purpose: it reads the root manifest, the inventory table and each
//! crate's manifest, and checks the claims the document makes about them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const CLASSES: &[&str] = &[
    "PRODUCT",
    "SUPPORTING",
    "INTEGRATION",
    "EXPERIMENT",
    "PROOF",
    "DEPRECATED",
    "UNPROVEN",
];
const SHIPPING_CLASSES: &[&str] = &["PRODUCT", "SUPPORTING", "INTEGRATION"];

struct Row {
    class: String,
    disposition: String,
    ci_level: String,
    reported_level: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn workspace_members() -> Vec<String> {
    let text = fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let members = text
        .split("members")
        .nth(1)
        .and_then(|rest| rest.split('[').nth(1))
        .and_then(|rest| rest.split(']').next())
        .expect("workspace members");
    members
        .split(',')
        .map(|m| m.trim().trim_matches('"'))
        .filter(|m| !m.is_empty())
        .map(|m| m.strip_prefix("crates/").unwrap_or(m).to_string())
        .collect()
}

/// The root manifest's `exclude` entries, as written.
fn excluded_paths() -> Vec<String> {
    let text = fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    text.split("exclude")
        .nth(1)
        .and_then(|rest| rest.split('[').nth(1))
        .and_then(|rest| rest.split(']').next())
        .map(|list| {
            list.split(',')
                .map(|m| m.trim().trim_matches('"').to_string())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Vendored third-party source (`vendor/`), excluded so that its own tests and benches are not
/// built here. It is not a Chip crate and is not inventoried as one.
const VENDOR: &str = "vendor";

/// Crates that live under `crates/` but are deliberately outside the root workspace (the root
/// manifest's `exclude`). They are still inventoried, and each must be an EXPERIMENT. Every other
/// `exclude` entry must be the vendored tree.
fn excluded_crates() -> Vec<String> {
    excluded_paths()
        .into_iter()
        .filter(|m| m != VENDOR)
        .map(|m| {
            m.strip_prefix("crates/")
                .unwrap_or_else(|| {
                    panic!("exclude entry {m} is neither under crates/ nor {VENDOR}")
                })
                .to_string()
        })
        .collect()
}

fn inventory() -> Vec<(String, Row)> {
    let text = fs::read_to_string(repo_root().join("docs/product/crates.md")).unwrap();
    let table = text
        .split("<!-- inventory:start -->")
        .nth(1)
        .and_then(|rest| rest.split("<!-- inventory:end -->").next())
        .expect("inventory markers");
    let mut rows = Vec::new();
    for line in table.lines().map(str::trim).filter(|l| l.starts_with('|')) {
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 5, "malformed inventory row: {line}");
        if cells[0] == "Crate" || cells[0].starts_with("---") {
            continue;
        }
        let name = cells[0].trim_matches('`').to_string();
        rows.push((
            name,
            Row {
                class: cells[1].to_string(),
                disposition: cells[2].to_string(),
                ci_level: cells[3].to_string(),
                reported_level: cells[4].to_string(),
            },
        ));
    }
    rows
}

fn is_level(s: &str) -> bool {
    matches!(s, "L0" | "L1" | "L2" | "L3" | "L4" | "L5")
}

#[test]
fn every_workspace_crate_is_in_the_inventory_exactly_once_and_nothing_else_is() {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (name, _) in inventory() {
        *counts.entry(name).or_default() += 1;
    }
    for member in workspace_members().into_iter().chain(excluded_crates()) {
        assert_eq!(
            counts.remove(&member),
            Some(1),
            "{member} must appear exactly once in docs/product/crates.md"
        );
    }
    assert!(
        counts.is_empty(),
        "inventory lists crates that are not workspace members: {:?}",
        counts.keys().collect::<Vec<_>>()
    );
}

#[test]
fn every_inventory_crate_exists() {
    for (name, _) in inventory() {
        let manifest = repo_root().join("crates").join(&name).join("Cargo.toml");
        let text = fs::read_to_string(&manifest)
            .unwrap_or_else(|_| panic!("{name}: {} does not exist", manifest.display()));
        assert!(
            text.contains(&format!("name = \"{name}\"")),
            "{name}: package name differs from its directory"
        );
    }
}

#[test]
fn every_crate_has_exactly_one_known_classification_and_disposition() {
    for (name, row) in inventory() {
        assert!(
            CLASSES.contains(&row.class.as_str()),
            "{name}: unknown classification {:?}",
            row.class
        );
        let allowed: &[&str] = if SHIPPING_CLASSES.contains(&row.class.as_str()) {
            &["KEEP", "FROZEN"]
        } else {
            &["FROZEN", "REJECTED"]
        };
        assert!(
            allowed.contains(&row.disposition.as_str()),
            "{name}: a {} crate must be {:?}, not {:?}",
            row.class,
            allowed,
            row.disposition
        );
    }
}

#[test]
fn every_crate_has_a_documented_validation_level() {
    for (name, row) in inventory() {
        assert!(
            is_level(&row.ci_level),
            "{name}: CI level {:?}",
            row.ci_level
        );
        assert!(
            is_level(&row.reported_level) || row.reported_level == "-",
            "{name}: reported level {:?}",
            row.reported_level
        );
    }
}

#[test]
fn only_chip_cli_links_crates_that_are_not_product_integration_or_supporting() {
    let rows: BTreeMap<String, Row> = inventory().into_iter().collect();
    for (name, row) in &rows {
        if !SHIPPING_CLASSES.contains(&row.class.as_str()) || name == "stack-agent" {
            continue;
        }
        let manifest =
            fs::read_to_string(repo_root().join("crates").join(name).join("Cargo.toml")).unwrap();
        // Only normal dependencies; dev-dependencies are test fixtures.
        let deps = manifest
            .split("[dependencies]")
            .nth(1)
            .map(|rest| rest.split("\n[").next().unwrap())
            .unwrap_or("");
        for line in deps.lines() {
            let dep = line.split('=').next().unwrap().trim();
            if let Some(other) = rows.get(dep) {
                assert!(
                    SHIPPING_CLASSES.contains(&other.class.as_str()),
                    "{name} ({}) depends on {dep} ({}), which has not earned product status",
                    row.class,
                    other.class
                );
            }
        }
    }
}

#[test]
fn an_excluded_crate_is_an_experiment_and_no_member_depends_on_it() {
    let rows: BTreeMap<String, Row> = inventory().into_iter().collect();
    for name in excluded_crates() {
        let row = rows
            .get(&name)
            .unwrap_or_else(|| panic!("{name} is not inventoried"));
        assert_eq!(
            row.class, "EXPERIMENT",
            "{name} is outside the workspace, so it must be an EXPERIMENT"
        );
        for member in workspace_members() {
            let manifest =
                fs::read_to_string(repo_root().join("crates").join(&member).join("Cargo.toml"))
                    .unwrap();
            assert!(
                !manifest.contains(&name),
                "{member} depends on the excluded crate {name}"
            );
        }
    }
}

/// Each vendored package records where it came from and the digest of its source, and the
/// source still matches that digest (CI checks the same digest before building).
#[test]
fn every_vendored_package_records_its_source_and_still_matches_it() {
    if !excluded_paths().iter().any(|m| m == VENDOR) {
        return;
    }
    let vendor = repo_root().join(VENDOR);
    let mut packages = 0;
    for entry in fs::read_dir(&vendor).unwrap().flatten() {
        let dir = entry.path();
        if !dir.join("Cargo.toml").is_file() {
            continue;
        }
        packages += 1;
        let notes = fs::read_to_string(dir.join("VENDORED.md"))
            .unwrap_or_else(|_| panic!("{} has no VENDORED.md", dir.display()));
        let field = |name: &str| {
            notes
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{name}: ")))
                .unwrap_or_else(|| panic!("{} VENDORED.md has no {name}", dir.display()))
                .trim()
                .to_string()
        };
        for name in ["source", "commit", "path"] {
            assert!(!field(name).is_empty());
        }
        assert_eq!(
            source_digest(&dir),
            field("src-digest"),
            "{}/src no longer matches the digest recorded in VENDORED.md",
            dir.display()
        );
    }
    assert!(packages > 0, "vendor/ is excluded but holds no package");
}

/// The SHA-256 of the sorted per-file `sha256sum` listing of `dir/src`, as VENDORED.md defines it.
fn source_digest(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push(relative);
            }
        }
    }
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut files = Vec::new();
    walk(dir, &dir.join("src"), &mut files);
    files.sort();
    let listing: String = files
        .iter()
        .map(|f| {
            format!(
                "{}  {f}\n",
                hex(&Sha256::digest(fs::read(dir.join(f)).unwrap()))
            )
        })
        .collect();
    hex(&Sha256::digest(listing.as_bytes()))
}

/// FeltDB is the published crate, once: no path dependency on a copy of it, and exactly one
/// `feltdb` package in the resolved graph, from the registry, with a checksum. A second copy would
/// mean two implementations of the file lock and the log in one build.
#[test]
fn feltdb_is_the_published_crate_and_there_is_one_of_it() {
    let root = repo_root();
    assert!(
        !root.join("vendor").join("feltdb").exists(),
        "vendor/feltdb is back"
    );
    for entry in fs::read_dir(root.join("crates")).unwrap().flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&manifest) else {
            continue;
        };
        for line in text
            .lines()
            .filter(|l| l.trim_start().starts_with("feltdb"))
        {
            assert!(
                !line.contains("path"),
                "{} depends on feltdb by path: {line}",
                manifest.display()
            );
        }
    }
    let lock = fs::read_to_string(root.join("Cargo.lock")).unwrap();
    let blocks: Vec<&str> = lock
        .split("[[package]]")
        .filter(|b| b.trim_start().starts_with("name = \"feltdb\""))
        .collect();
    assert_eq!(blocks.len(), 1, "one feltdb package in Cargo.lock");
    assert!(
        blocks[0].contains("source = \"registry+https://github.com/rust-lang/crates.io-index\"")
            && blocks[0].contains("checksum = "),
        "feltdb is not a checksummed registry package: {}",
        blocks[0]
    );
    assert!(blocks[0].contains("version = \"0.2.0\""), "{}", blocks[0]);
}
