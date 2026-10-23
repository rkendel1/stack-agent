#!/bin/sh
# Build the stack-agent release artifact: one `stack-agent` executable (Chip + FX, one workspace,
# one Cargo.lock) plus a manifest that records the commit and the crate versions it was built from.
#
#   scripts/package-release.sh [output-directory]    (default: dist/)
#
# Writes <out>/stack-agent-<version>-<target>.tar.gz and <out>/stack-agent-<version>-<target>.tar.gz.sha256
# (`<hash>  <file name>`, the format `sha256sum -c` and `shasum -a 256 -c` read).
#
# Works on macOS and Linux. Needs cargo, rustc, git and python3 (all present where the binary is
# built); nothing here uses GNU-only options.
#
# Environment:
#   STACK_AGENT_BINARY   package this already-built executable instead of building one. The release
#                        workflow builds once and packages that binary; the determinism test packages
#                        one binary twice.
#   RELEASE_TAG          when set (the release workflow sets it from the pushed tag), it must equal
#                        `v<stack-agent crate version>`; a mismatch stops the script before packaging.
#   SOURCE_DATE_EPOCH    the modification time recorded in the archive; the commit time by default.
#
# What "deterministic" means here: the same executable, the same commit and the same Python
# produce a byte-identical archive. Entries are sorted, owned by no one, with fixed modes and times,
# and the gzip header carries no name or time. It does not mean the *executable* is bit-for-bit
# reproducible from source; that is measured separately and is not claimed.
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
out=${1:-$root/dist}
mkdir -p "$out"
out=$(CDPATH='' cd -- "$out" && pwd)
cd "$root"

# The checksum of a file, as the lower-case hex digest only.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    echo "package-release: neither sha256sum nor shasum is available" >&2
    exit 1
  fi
}

version=$(cargo metadata --format-version 1 --no-deps --locked | python3 -I -c '
import json, sys
meta = json.load(sys.stdin)
print({p["name"]: p["version"] for p in meta["packages"]}["stack-agent"])')

# A release is cut from a tag that names the version the binary reports.
if [ -n "${RELEASE_TAG:-}" ] && [ "$RELEASE_TAG" != "v$version" ]; then
  echo "package-release: tag $RELEASE_TAG does not match the stack-agent crate version v$version; nothing was packaged" >&2
  exit 1
fi

if [ -n "${STACK_AGENT_BINARY:-}" ]; then
  binary=$STACK_AGENT_BINARY
  [ -x "$binary" ] || { echo "package-release: $binary is not an executable file" >&2; exit 1; }
else
  # --locked: the artifact is built from exactly the dependencies Cargo.lock names.
  cargo build --release --locked --bin stack-agent
  binary=$root/target/release/stack-agent
fi

target=$(rustc -vV | sed -n 's/^host: //p')
commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)
dirty=false
if [ "$commit" != unknown ] && [ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]; then dirty=true; fi
stamp=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || echo 0)}

name="stack-agent-$version-$target"
stage=$(mktemp -d "${TMPDIR:-/tmp}/stack-agent-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
mkdir -p "$stage/$name"

cp "$binary" "$stage/$name/stack-agent"
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
        "diagnostics": {"command": "doctor", "output": "stack-agent.doctor.v1", "exit_codes": [0, 2, 3]},
        "service": {"command": "serve", "flags": ["--host", "--port", "--max-concurrent-work", "--max-queued-work"]},
        "capability_worker": {"command": "capability-exec", "arguments": ["--root", "<project directory>"],
                              "request_env": "CHIP_CAPABILITY_REQUEST", "protocol": 1},
        "environment_contract": "chip-core EnvironmentProvider / WorkEnvironment",
    },
    # The executable is not code-signed or notarized by this pipeline. See docs/product/release.md.
    "signing": "none",
    "not": ["npm Chip/Eve", "npm FX/Zig"],
}
json.dump(manifest, open(sys.argv[1], "w"), indent=2, sort_keys=True)
open(sys.argv[1], "a").write("\n")
' "$stage/$name/manifest.json"

# The archive is written by Python's tarfile rather than `tar`: BSD tar (macOS) and GNU tar (Linux)
# differ in the options that make an archive deterministic, and this is the same on both.
(
  cd "$stage"
  python3 -I - "$name" "$stamp" "$name.tar.gz" <<'PY'
import gzip, io, os, sys, tarfile

name, stamp, archive = sys.argv[1], int(sys.argv[2]), sys.argv[3]

def entry(path, directory, executable):
    info = tarfile.TarInfo(path)
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = stamp
    if directory:
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
    else:
        info.mode = 0o755 if executable else 0o644
        info.size = os.path.getsize(path)
    return info

raw = io.BytesIO()
with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
    tar.addfile(entry(name, True, False))
    for member in sorted(os.listdir(name)):
        path = os.path.join(name, member)
        with open(path, "rb") as f:
            tar.addfile(entry(path, False, member == "stack-agent"), f)
# mtime=0 and an empty file name keep the gzip header free of anything that varies.
with open(archive, "wb") as out:
    with gzip.GzipFile(filename="", mode="wb", fileobj=out, compresslevel=9, mtime=0) as gz:
        gz.write(raw.getvalue())
PY
)
mv "$stage/$name.tar.gz" "$out/$name.tar.gz"
printf '%s  %s\n' "$(sha256_of "$out/$name.tar.gz")" "$name.tar.gz" > "$out/$name.tar.gz.sha256"
echo "$out/$name.tar.gz"
