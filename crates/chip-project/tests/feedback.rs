//! Actionable feedback from `project.read`, `project.list` and `project.search`.
//!
//! The runtime already knows which directory is nearest to a path that does not exist, how far a
//! bounded search got, and where a match lives. These tests establish that it says so, and that
//! saying so widens nothing: a hint is exactly what `project.list` would show, a refused path is
//! refused as before, and the same request still gets the same observation (the stuck guard
//! compares observations, so a hint that varied between identical requests would disable it).

use std::collections::BTreeMap;
use std::path::PathBuf;

use chip_core::{
    ExecutionError, ExecutionId, ExecutionRequest, ExecutionResult, ExecutionStatus, Executor,
    InputValue,
};
use chip_project::{
    MAX_HINT_ENTRIES, MAX_MATCHES, MAX_READ_BYTES, MAX_SEARCH_FILES, ProjectExecutor,
};

struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("chip-feedback-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, outside) = (base.join("project"), base.join("outside"));
    for d in ["src", "tests", "docs", ".git", "target/debug"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() {}\npub fn beta() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/util.rs"), "pub fn helper() { /* beta */ }\n").unwrap();
    std::fs::write(root.join("tests/a.rs"), "fn t() { beta(); }\n").unwrap();
    std::fs::write(root.join(".git/config"), "beta = secret\n").unwrap();
    std::fs::write(root.join(".env"), "beta=secret\n").unwrap();
    std::fs::write(outside.join("secret.txt"), "beta outside\n").unwrap();
    Fixture { root, outside }
}

fn inputs(pairs: &[(&str, &str)]) -> BTreeMap<String, InputValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), InputValue::Text(v.to_string())))
        .collect()
}

async fn run(
    p: &ProjectExecutor,
    capability: &str,
    given: BTreeMap<String, InputValue>,
) -> Result<ExecutionResult, ExecutionError> {
    p.execute(ExecutionRequest::new(ExecutionId::new("e"), capability).with_inputs(given))
        .await
}

fn head(r: &ExecutionResult) -> serde_json::Value {
    serde_json::from_str(r.output.lines().next().unwrap()).unwrap()
}

fn rows(r: &ExecutionResult) -> Vec<String> {
    r.output.lines().skip(2).map(str::to_string).collect()
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

// ---- a path that does not exist ----------------------------------------------------------------

#[tokio::test]
async fn a_missing_path_names_the_nearest_existing_directory_and_exactly_its_listing() {
    let f = fixture("missing");
    let p = ProjectExecutor::new(&f.root);

    // Three levels down, only `src` exists.
    let r = run(
        &p,
        "project.read",
        inputs(&[("path", "src/nothing/deeper.rs")]),
    )
    .await
    .unwrap();
    assert_eq!(r.status, ExecutionStatus::Failure);
    let h = head(&r);
    assert_eq!(h["error"], "not_found");
    assert_eq!(h["nearest_existing_directory"], "src");
    assert_eq!(h["missing_component"], "nothing");
    let listed = run(&p, "project.list", inputs(&[("path", "src")]))
        .await
        .unwrap();
    assert_eq!(
        strings(&h["entries"]),
        rows(&listed),
        "the hint is the listing"
    );
    assert_eq!(h["entries_total"], 2);

    // Directly under the root: the root's listing, which never shows reserved names.
    let r = run(&p, "project.read", inputs(&[("path", "source/lib.rs")]))
        .await
        .unwrap();
    let h = head(&r);
    assert_eq!(h["nearest_existing_directory"], ".");
    let listed = run(&p, "project.list", BTreeMap::new()).await.unwrap();
    assert_eq!(strings(&h["entries"]), rows(&listed));
    for withheld in [".env", ".git"] {
        assert!(
            !r.output.contains(withheld),
            "{withheld} must not be named: {}",
            r.output
        );
    }
    assert!(
        !r.output.contains(f.root.to_str().unwrap()),
        "no host path: {}",
        r.output
    );
}

#[tokio::test]
async fn list_and_search_give_the_same_help_for_a_missing_directory() {
    let f = fixture("missing-dir");
    let p = ProjectExecutor::new(&f.root);
    for (capability, given) in [
        ("project.list", inputs(&[("path", "docs/missing")])),
        (
            "project.search",
            inputs(&[("query", "beta"), ("path", "docs/missing")]),
        ),
    ] {
        let r = run(&p, capability, given).await.unwrap();
        let h = head(&r);
        assert_eq!(h["error"], "not_found", "{capability}");
        assert_eq!(h["nearest_existing_directory"], "docs", "{capability}");
        assert_eq!(h["entries_total"], 0, "{capability}: docs is empty");
    }
}

#[tokio::test]
async fn a_name_that_resembles_the_missing_one_is_suggested() {
    let f = fixture("similar");
    let p = ProjectExecutor::new(&f.root);
    let r = run(&p, "project.read", inputs(&[("path", "test/a.rs")]))
        .await
        .unwrap();
    assert_eq!(strings(&head(&r)["similar"]), ["tests"]);
    // Nothing resembles `zzz`, so nothing is suggested.
    let r = run(&p, "project.read", inputs(&[("path", "zzz/a.rs")]))
        .await
        .unwrap();
    assert!(head(&r).get("similar").is_none());
}

#[tokio::test]
async fn a_hint_is_bounded_like_a_listing() {
    let f = fixture("bounded");
    for i in 0..(MAX_HINT_ENTRIES + 25) {
        std::fs::write(f.root.join(format!("f{i:03}.txt")), "x").unwrap();
    }
    let p = ProjectExecutor::new(&f.root);
    let h = head(
        &run(&p, "project.read", inputs(&[("path", "nothing.rs")]))
            .await
            .unwrap(),
    );
    assert_eq!(strings(&h["entries"]).len(), MAX_HINT_ENTRIES);
    assert!(h["entries_total"].as_u64().unwrap() > MAX_HINT_ENTRIES as u64);
}

#[tokio::test]
async fn the_same_failing_request_gets_the_same_observation_every_time() {
    // The stuck guard compares observations. A hint that differed between identical requests would
    // make a loop look like progress.
    let f = fixture("deterministic");
    let p = ProjectExecutor::new(&f.root);
    let first = run(&p, "project.read", inputs(&[("path", "src/nothing.rs")]))
        .await
        .unwrap();
    for _ in 0..3 {
        let again = run(&p, "project.read", inputs(&[("path", "src/nothing.rs")]))
            .await
            .unwrap();
        assert_eq!(again.output, first.output);
    }
    let a = run(&p, "project.search", inputs(&[("query", "beta")]))
        .await
        .unwrap();
    let b = run(&p, "project.search", inputs(&[("query", "beta")]))
        .await
        .unwrap();
    assert_eq!(a.output, b.output);
}

#[tokio::test]
async fn a_refused_path_is_refused_exactly_as_before_and_gets_no_hint() {
    let f = fixture("refused");
    let p = ProjectExecutor::new(&f.root);
    for path in [
        "../outside/secret.txt",
        "/etc/passwd",
        ".env",
        ".git/config",
        "a//b",
    ] {
        let r = run(&p, "project.read", inputs(&[("path", path)])).await;
        match r {
            Err(_) => {}
            Ok(r) => assert!(
                r.status != ExecutionStatus::Success && head(&r).get("entries").is_none(),
                "{path}: {}",
                r.output
            ),
        }
    }
    #[cfg(unix)]
    {
        // A link out of the project is refused; its target is never listed or named.
        std::os::unix::fs::symlink(&f.outside, f.root.join("link")).unwrap();
        let r = run(&p, "project.read", inputs(&[("path", "link/secret.txt")])).await;
        match r {
            Err(_) => {}
            Ok(r) => assert!(
                r.status != ExecutionStatus::Success
                    && !r.output.contains("secret.txt")
                    && head(&r).get("entries").is_none(),
                "{}",
                r.output
            ),
        }
    }
}

#[tokio::test]
async fn reading_a_directory_and_listing_a_file_say_what_to_use() {
    let f = fixture("kinds");
    let p = ProjectExecutor::new(&f.root);
    let r = run(&p, "project.read", inputs(&[("path", "src")]))
        .await
        .unwrap();
    assert_eq!(head(&r)["error"], "not_a_file");
    assert!(head(&r)["hint"].as_str().unwrap().contains("project.list"));
    let r = run(&p, "project.list", inputs(&[("path", "src/lib.rs")]))
        .await
        .unwrap();
    assert_eq!(head(&r)["error"], "not_a_directory");
    assert!(head(&r)["hint"].as_str().unwrap().contains("project.read"));
}

// ---- how far a search got ----------------------------------------------------------------------

fn many_files(root: &std::path::Path, dir: &str, count: usize) {
    std::fs::create_dir_all(root.join(dir)).unwrap();
    for i in 0..count {
        std::fs::write(
            root.join(dir).join(format!("f{i:03}.txt")),
            "nothing here\n",
        )
        .unwrap();
    }
}

#[tokio::test]
async fn a_search_that_hit_the_file_limit_says_so_and_what_it_did_not_reach() {
    let f = fixture("limit");
    // Walk order is alphabetical: `a` is searched fully, the limit falls inside `b`, `c` and
    // `z.txt` are never reached. `src`, `tests` and `docs` of the fixture sort after `b`.
    let third = MAX_SEARCH_FILES / 2 + 50;
    many_files(&f.root, "a", third);
    many_files(&f.root, "b", third);
    many_files(&f.root, "c", 10);
    std::fs::write(f.root.join("z.txt"), "x\n").unwrap();
    let p = ProjectExecutor::new(&f.root);

    let r = run(&p, "project.search", inputs(&[("query", "absent_needle")]))
        .await
        .unwrap();
    let h = head(&r);
    assert_eq!(h["truncated"], true);
    assert_eq!(h["exhaustive"], false);
    assert_eq!(h["stopped_by"], "file_limit");
    assert_eq!(h["file_limit"], MAX_SEARCH_FILES as u64);
    assert_eq!(h["files_examined"], MAX_SEARCH_FILES as u64);
    // Names sort bytewise, so `Cargo.toml` (capital C) comes before `a` and was searched fully.
    assert_eq!(strings(&h["searched_fully"]), ["Cargo.toml", "a"]);
    assert_eq!(strings(&h["stopped_inside"]), ["b"]);
    let not_reached = strings(&h["not_reached"]);
    for unreached in ["c", "docs", "src", "tests", "z.txt"] {
        assert!(
            not_reached.contains(&unreached.to_string()),
            "{unreached}: {not_reached:?}"
        );
    }
    let hint = h["hint"].as_str().unwrap();
    assert!(
        hint.contains("NOT EXHAUSTIVE") && hint.contains("proves nothing"),
        "{hint}"
    );
    assert_eq!(h["matches"], 0);
}

#[tokio::test]
async fn a_search_that_covered_everything_says_the_query_is_not_there() {
    let f = fixture("exhaustive");
    let p = ProjectExecutor::new(&f.root);
    let r = run(&p, "project.search", inputs(&[("query", "absent_needle")]))
        .await
        .unwrap();
    let h = head(&r);
    assert_eq!(h["truncated"], false);
    assert_eq!(h["exhaustive"], true);
    assert!(h.get("stopped_by").is_none());
    assert!(h.get("not_reached").is_none());
    assert!(
        h["hint"]
            .as_str()
            .unwrap()
            .contains("No file contains this query")
    );
    // `target` was excluded by rule and is named as such, so "everything" is not overstated.
    assert_eq!(strings(&h["excluded_by_rule"]), ["target"]);
}

#[tokio::test]
async fn a_search_that_hit_the_match_limit_says_which_limit() {
    let f = fixture("matches");
    let lines: String = (0..MAX_MATCHES + 20)
        .map(|i| format!("needle {i}\n"))
        .collect();
    std::fs::write(f.root.join("src/many.rs"), lines).unwrap();
    let p = ProjectExecutor::new(&f.root);
    let r = run(&p, "project.search", inputs(&[("query", "needle")]))
        .await
        .unwrap();
    let h = head(&r);
    assert_eq!(h["stopped_by"], "match_limit");
    assert_eq!(h["exhaustive"], false);
    assert_eq!(h["matches"], MAX_MATCHES as u64);
    assert!(h["hint"].as_str().unwrap().contains("NOT EXHAUSTIVE"));
}

// ---- turning a match into a read ---------------------------------------------------------------

#[tokio::test]
async fn every_read_hint_is_a_project_read_that_works() {
    let f = fixture("read-hints");
    // A file too large for one whole read, with its only match far from the start.
    let big: String = (0..4000)
        .map(|i| {
            if i == 3500 {
                "the needle is here\n".to_string()
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    assert!(big.len() > MAX_READ_BYTES);
    std::fs::write(f.root.join("src/big.txt"), &big).unwrap();
    let p = ProjectExecutor::new(&f.root);

    let r = run(&p, "project.search", inputs(&[("query", "needle")]))
        .await
        .unwrap();
    let h = head(&r);
    assert_eq!(h["files_with_matches"], 1);
    let hint = &h["read_hints"][0];
    assert_eq!(hint["path"], "src/big.txt");
    assert_eq!(hint["first_line"], 3501);
    assert_eq!(hint["file_bytes"], big.len() as u64);
    // The offset is the start of the matching line, and the window holds whole lines.
    let offset = hint["read"]["offset"].as_u64().unwrap() as usize;
    let length = hint["read"]["length"].as_u64().unwrap() as usize;
    assert_eq!(&big[offset..offset + 19], "the needle is here\n");
    assert!((19..=MAX_READ_BYTES).contains(&length));
    assert!(big[offset..offset + length].ends_with('\n'));

    // Executing exactly what the hint says succeeds and returns the match.
    let mut given = BTreeMap::new();
    given.insert("path".to_string(), InputValue::Text("src/big.txt".into()));
    given.insert("offset".to_string(), InputValue::Integer(offset as i64));
    given.insert("length".to_string(), InputValue::Integer(length as i64));
    let read = run(&p, "project.read", given).await.unwrap();
    assert_eq!(read.status, ExecutionStatus::Success, "{}", read.output);
    assert!(
        read.output.contains("the needle is here"),
        "{}",
        read.output
    );

    // A small file needs only its path, and that read works too.
    let r = run(&p, "project.search", inputs(&[("query", "helper")]))
        .await
        .unwrap();
    let hint = &head(&r)["read_hints"][0];
    assert_eq!(hint["read"], serde_json::json!({"path": "src/util.rs"}));
    let read = run(&p, "project.read", inputs(&[("path", "src/util.rs")]))
        .await
        .unwrap();
    assert_eq!(read.status, ExecutionStatus::Success);
}

#[tokio::test]
async fn read_hints_never_name_a_file_the_search_did_not_match() {
    let f = fixture("hint-subset");
    let p = ProjectExecutor::new(&f.root);
    let r = run(&p, "project.search", inputs(&[("query", "beta")]))
        .await
        .unwrap();
    let h = head(&r);
    let matched: Vec<String> = rows(&r)
        .iter()
        .map(|row| row.split(':').next().unwrap().to_string())
        .collect();
    for hint in h["read_hints"].as_array().unwrap() {
        let path = hint["path"].as_str().unwrap();
        assert!(
            matched.iter().any(|m| m == path),
            "{path} was not matched: {matched:?}"
        );
    }
    // The reserved files that contain the query are not searched and so not hinted.
    assert!(
        !r.output.contains(".env") && !r.output.contains(".git"),
        "{}",
        r.output
    );
}
