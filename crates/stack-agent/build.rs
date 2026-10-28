//! Records which source this binary was built from, so every report can say so.
//!
//! `STACK_AGENT_COMMIT` is the short git commit when the build runs inside a checkout and `unknown`
//! otherwise (a source tarball, `cargo install` from a registry); `STACK_AGENT_DIRTY` is `dirty` when
//! tracked files differ from that commit. Nothing is fetched, and a missing `git` is not an error.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(s) if s.is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };
    println!("cargo:rustc-env=STACK_AGENT_COMMIT={commit}");
    println!("cargo:rustc-env=STACK_AGENT_DIRTY={dirty}");
    println!(
        "cargo:rustc-env=STACK_AGENT_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=STACK_AGENT_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_default()
    );
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}
