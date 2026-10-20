# Vendored FeltDB

`chip-agent-state` depends on FeltDB's native Rust library through this directory. Building stack-agent
therefore needs no access to the FeltDB repository and no token.

source: https://github.com/rkendel1/flow_db
commit: b24481360f7c10564ae09fcc5bbf7802725eccd9
path: crates/feltdb
src-digest: 277892e9bf39f7a9734b44f6187d2a123e9ffd7a0fdf7b4568801ed3de39d146

## What is here

| File | Origin |
| --- | --- |
| `src/` | `crates/feltdb/src` at the commit above, byte for byte (119 files). |
| `LICENSE` | the repository's `LICENSE` (MIT) at the commit above. |
| `Cargo.toml` | written for this copy, as follows. |
| `rustfmt.toml` | written for this copy (`disable_all_formatting`). `cargo fmt --all` also formats path dependencies, and the source is kept as upstream wrote it. |

`Cargo.toml` differs from upstream's `crates/feltdb/Cargo.toml` in four ways:

* the `version` (`0.2.0`) and `edition` (`2021`) upstream inherits from its workspace are written out;
* upstream's benches and dev-dependencies are omitted, because their files are not vendored and
  nothing here builds them;
* `[lints.rust]` allows the `unused` group and `private_interfaces`. Cargo caps lints for git
  dependencies but not for path dependencies, and these are upstream's own warnings;
* `publish = false`.

The runtime dependencies are upstream's exactly. The root `Cargo.toml` excludes `vendor/` from the
workspace, so FeltDB's own tests are not built or run here; they run in its repository.

## Checking it

`src-digest` is the SHA-256 of the sorted per-file `sha256sum` listing of `src/`:

```sh
cd vendor/feltdb && find src -type f | LC_ALL=C sort | xargs sha256sum | sha256sum
```

CI recomputes it and fails if `src/` no longer matches. Against a checkout of the FeltDB repository:

```sh
git -C flow_db archive b24481360f7c10564ae09fcc5bbf7802725eccd9 crates/feltdb/src | tar -x -C /tmp/feltdb
diff -r /tmp/feltdb/crates/feltdb/src vendor/feltdb/src
```

## Updating it

Moving to another FeltDB commit changes the agent-state authority stack-agent runs against, so do it
in its own PR:

1. Replace `src/` and `LICENSE` from the new commit with `git archive`, as above.
2. Copy any change to upstream's runtime `[dependencies]` into `Cargo.toml`.
3. Record the new `commit` and `src-digest` here.
4. Run `cargo check --workspace --all-targets --locked` and the agent-state tests
   (`cargo test -p chip-agent-state`, and `cargo test -p stack-agent --test agent_state_cli --test durability_bench`).
5. If the agent-transition contract changed, `chip-agent-state` refuses it (`check_contract`) until it
   is reviewed and adopted.
