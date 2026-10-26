//! What this crate is allowed to be, pinned so it stays that way.
//!
//! It is an adapter: it speaks cli2's protocol and implements Chip's capability traits, and it
//! adds no loop, no model, no store and no second way to change a file.

use std::fs;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn repo_root() -> PathBuf {
    crate_dir().join("../..")
}

fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(crate_dir().join("src")).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "rs") {
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&p).unwrap(),
            ));
        }
    }
    out
}

/// Only code, not the prose that explains what it does not do.
fn code(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("//") || t.starts_with("///") || t.starts_with("//!"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normal_dependencies(manifest: &str) -> Vec<String> {
    manifest
        .split("[dependencies]")
        .nth(1)
        .map(|rest| rest.split("\n[").next().unwrap())
        .unwrap_or("")
        .lines()
        .filter_map(|l| l.split('=').next())
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

#[test]
fn it_depends_on_chips_contracts_and_the_standard_plumbing_and_nothing_else() {
    let manifest = fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let mut deps = normal_dependencies(&manifest);
    deps.sort();
    assert_eq!(
        deps,
        ["async-trait", "chip-core", "serde_json", "tokio"],
        "no model, no FX, no storage, no HTTP client, and no cli2 crate"
    );
}

#[test]
fn nothing_in_the_workspace_links_cli2() {
    // cli2 is a separate program reached over its protocol; no Chip crate is built against it.
    for entry in fs::read_dir(repo_root().join("crates")).unwrap() {
        let p = entry.unwrap().path().join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        for dep in normal_dependencies(&text) {
            assert!(!dep.starts_with("cli2"), "{} depends on {dep}", p.display());
        }
    }
    let lock = fs::read_to_string(repo_root().join("Cargo.lock")).unwrap();
    assert!(
        !lock.lines().any(|l| l.starts_with("name = \"cli2")),
        "Cargo.lock must hold no cli2 package"
    );
}

#[test]
fn it_never_touches_the_cli2_task_controller_or_keeps_state_of_its_own() {
    for (name, text) in sources() {
        let c = code(&text);
        // Chip owns the loop; cli2's own development-task controller is not used.
        assert!(!c.contains("devtask"), "{name} uses cli2's task controller");
        // No store, no files: cli2's journal is the evidence, and Chip's state is Chip's.
        for forbidden in [
            "rusqlite",
            "feltdb",
            "fs::write",
            "File::create",
            "OpenOptions",
            "create_dir",
            "remove_file",
        ] {
            assert!(
                !c.contains(forbidden),
                "{name} persists something ({forbidden})"
            );
        }
    }
}

#[test]
fn it_offers_no_way_to_read_or_write_project_files() {
    // Chip's project.read/project.write carry the preconditions, confinement and tamper checks.
    for (name, text) in sources() {
        let c = code(&text);
        for forbidden in [
            "plan.edit",
            "file.read",
            "\"project.write\"",
            "\"project.read\"",
        ] {
            assert!(
                !c.contains(forbidden),
                "{name} routes around Chip's file capabilities ({forbidden})"
            );
        }
    }
}

#[test]
fn the_model_has_no_path_to_an_approval() {
    // Approval is decided by an Approver the host supplies; the only input the model can give
    // `cli2.run` is a task id, and the digest approved is the one cli2 reported for the plan.
    let lib = code(&fs::read_to_string(crate_dir().join("src/lib.rs")).unwrap());
    assert!(
        lib.contains("cfg.approver.review"),
        "approval is asked of the host's approver"
    );
    assert!(
        !lib.contains("inputs.get(\"digest\")") && !lib.contains("inputs.get(\"approval"),
        "no input can carry an approval"
    );
    // The approved digest is the plan's own, read from cli2's answer.
    assert!(lib.contains("\"plan_digest\": digest"));
}
