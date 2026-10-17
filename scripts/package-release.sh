#!/bin/sh
# Build the stack-agent release artifact: one `stack-agent` executable (Chip + FX, one workspace,
# one Cargo.lock) plus a manifest that records the commit and the crate versions it was built from.
#
#   scripts/package-release.sh [output-directory]    (default: dist/)
#
# Writes <out>/stack-agent-<version>-<target>.tar.gz and <out>/stack-agent-<version>-<target>.tar.gz.sha256.
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
out=${1:-$root/dist}
mkdir -p "$out"
out=$(CDPATH='' cd -- "$out" && pwd)
cd "$root"

# --locked: the artifact is built from exactly the dependencies Cargo.lock names.
cargo build --release --locked --bin stack-agent

target=$(rustc -vV | sed -n 's/^host: //p')
commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)
dirty=false
if [ "$commit" != unknown ] && [ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]; then dirty=true; fi
stamp=$(git log -1 --format=%ct 2>/dev/null || echo 0)

version=$(cargo metadata --format-version 1 --no-deps --locked | python3 -I -c '
import json, sys
meta = json.load(sys.stdin)
print({p["name"]: p["version"] for p in meta["packages"]}["stack-agent"])')
name="stack-agent-$version-$target"
stage=$(mktemp -d "${TMPDIR:-/tmp}/stack-agent-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
mkdir -p "$stage/$name"

cp "$root/target/release/stack-agent" "$stage/$name/stack-agent"
cp "$root/LICENSE" "$stage/$name/LICENSE"
cp "$root/LICENSE-APACHE" "$stage/$name/LICENSE-APACHE"
cp "$root/README.md" "$stage/$name/README.md"
cp "$root/RELEASE_NOTES.md" "$stage/$name/RELEASE_NOTES.md"

cargo metadata --format-version 1 --no-deps --locked | CHIP_COMMIT="$commit" CHIP_DIRTY="$dirty" CHIP_TARGET="$target" \
  CHIP_RUSTC="$(rustc --version)" python3 -I -c '
import json, os, sys
meta = json.load(sys.stdin)
v = {p["name"]: p["version"] for p in meta["packages"]}
manifest = {
    "format": "stack-agent.release@1",
    "product": "stack-agent",
    "agent": "Rust Chip over Rust FX",
    "version": v["stack-agent"],
    "commit": os.environ["CHIP_COMMIT"],
    "dirty": os.environ["CHIP_DIRTY"] == "true",
    "target": os.environ["CHIP_TARGET"],
    "rustc": os.environ["CHIP_RUSTC"],
    # Both components come from this one commit; their crate versions are recorded for reference.
    "stack": {
        "rust_chip": {k: v[k] for k in ("stack-agent", "chip-core", "chip-inference", "chip-remote-env", "chip-project", "chip-pax")},
        "rust_fx": {k: v[k] for k in ("chip-fx", "chip-fx-native", "fx-core", "fx-single-attempt", "fx-native-provider")},
    },
    # What a program that hosts or launches this artifact may rely on.
    "interfaces": {
        "executable": "stack-agent",
        "version_output": "stack-agent <version>",
        "service": {"command": "serve", "flags": ["--host", "--port", "--max-concurrent-work", "--max-queued-work"]},
        "capability_worker": {"command": "capability-exec", "arguments": ["--root", "<project directory>"],
                              "request_env": "CHIP_CAPABILITY_REQUEST", "protocol": 1},
        "environment_contract": "chip-core EnvironmentProvider / WorkEnvironment",
    },
    "not": ["npm Chip/Eve", "npm FX/Zig"],
}
json.dump(manifest, open(sys.argv[1], "w"), indent=2, sort_keys=True)
open(sys.argv[1], "a").write("\n")
' "$stage/$name/manifest.json"

# Archive: create tarball and compress.
# Note: macOS tar (BSD tar) has limited options compared to GNU tar.
(
  cd "$stage"
  tar -cf "$name.tar" "$name"
  gzip -n -9 "$name.tar"
)
mv "$stage/$name.tar.gz" "$out/$name.tar.gz"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
