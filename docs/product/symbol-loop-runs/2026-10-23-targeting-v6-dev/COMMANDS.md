# Commands (targeting v6 development run)

`<S>` is the session scratchpad. Fixtures live outside the repository: `<S>/heldout-fixtures` (v3,
f01–f12) and `<S>/heldout-v5-fixtures`. The v5 set is identical to the committed archive
`../2026-10-23-targeting-v5-held-out/heldout-v5-fixtures.tar.gz` (checked with `diff -r`). T1 and
T3 are the evaluation-only candidates prepared earlier.

## Gates (`logs/gates.log`)

```
cargo build -p stack-agent --offline --example symbol_loop
cargo test -p stack-agent --offline --test inventory --test product_path
cargo test -p chip-core --offline --test forbidden_dependencies
cargo test -p chip-symbol-loop --offline
cargo clippy -p chip-symbol-loop --offline --all-targets
cargo fmt --all -- --check
(cd ../scorer-v4 && python3 -I -B test_score_v4.py)
(cd ../2026-10-23-targeting-v5-held-out && python3 -I -B test_analyze_v5.py)
```

## Binary

```
cargo build -p chip-symbol-loop --offline --example rank_target
cp target/debug/examples/rank_target <S>/rank_target.v6dev
```

The hashes of the binary, the sources and the scripts are in `SOURCES.sha256`.

## Development diagnostics (`logs/dev-runs.log`, `raw-v3/`, `raw-v5/`, `raw-t1-t3/`)

```
TMPDIR=<S>/v6-work python3 -I -B ../scorer-v4/score_v4.py run <S>/rank_target.v6dev <S>/heldout-fixtures raw-v3
TMPDIR=<S>/v6-work python3 -I -B ../scorer-v4/score_v4.py run <S>/rank_target.v6dev <S>/heldout-v5-fixtures raw-v5
(cd <S>/t3 && CARGO_TARGET_DIR=<S>/t3/target <S>/rank_target.v6dev <S>/t3/candidate \
    cargo test --offline -p compute-policy --test policy_version_acceptance) > raw-t1-t3/T3.json
(cd <S>/t1 && CARGO_TARGET_DIR=<S>/t1/target <S>/rank_target.v6dev <S>/t1/candidate \
    cargo test --offline -p compute-provider --test target_auth_acceptance) > raw-t1-t3/T1.json
python3 -I -B dev_analyze.py v3 <S>/heldout-fixtures raw-v3 v5 <S>/heldout-v5-fixtures raw-v5 > dev-analysis.json
python3 -I -B ranking_ablation.py <S>/heldout-fixtures raw-v3 <S>/heldout-v5-fixtures raw-v5 > ablation-final.json
```

## The basis of the ranking decision (`ranking-basis/`)

`raw-v3`/`raw-v5` were produced by the v6 code **before** the ranking changes. The ranking changes
were chosen from `ranking-basis/ablation.json`, which `ranking_ablation.py` computed from those
outputs.
