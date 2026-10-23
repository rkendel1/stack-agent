#!/bin/sh
# Tests for scripts/package-release.sh that need no build: it packages a stand-in executable.
#
#   scripts/test-package.sh
#
# Pins: identical input gives a byte-identical archive and checksum file; the checksum file verifies
# with the portable tool of this machine; archive entries are sorted, owned by no one and carry the
# fixed time; the manifest names the target and version; a release tag that does not match the crate
# version stops packaging before anything is written.
#
# These tests say nothing about whether the real executable is reproducible from source.
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/stack-agent-test-package.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
passed=0
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { passed=$((passed + 1)); echo "ok   $*"; }

cd "$root"
version=$(cargo metadata --format-version 1 --no-deps --locked | python3 -I -c '
import json, sys
print({p["name"]: p["version"] for p in json.load(sys.stdin)["packages"]}["stack-agent"])')
bin=$work/stack-agent
printf '#!/bin/sh\necho "stack-agent %s"\n' "$version" > "$bin"
chmod 755 "$bin"

STACK_AGENT_BINARY=$bin
SOURCE_DATE_EPOCH=1700000000
export STACK_AGENT_BINARY SOURCE_DATE_EPOCH

# 1. Determinism: the same executable twice, in different places and at different times.
a=$(sh scripts/package-release.sh "$work/a")
sleep 2
b=$(sh scripts/package-release.sh "$work/b")
cmp "$a" "$b" || fail "two packagings of one executable differ"
cmp "$a.sha256" "$b.sha256" || fail "the checksum files differ"
pass "identical input gives a byte-identical archive and checksum file"

# 2. The checksum file verifies, in the format both tools read.
(
  cd "$work/a"
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -c ./*.sha256 >/dev/null
  else shasum -a 256 -c ./*.sha256 >/dev/null; fi
) || fail "the checksum file does not verify"
case $(cat "$a.sha256") in
  *"  stack-agent-$version-"*".tar.gz") ;;
  *) fail "the checksum file is not '<hash>  <file name>': $(cat "$a.sha256")" ;;
esac
pass "the checksum file verifies and names the archive without a path"

# 3. The archive's entries.
python3 -I - "$a" "$version" <<'PY' || fail "the archive layout is wrong"
import gzip, sys, tarfile
path, version = sys.argv[1], sys.argv[2]
raw = open(path, "rb").read()
# The gzip header carries no name and no time.
assert raw[3] == 0 and raw[4:8] == b"\0\0\0\0", raw[:10]
with tarfile.open(path) as tar:
    members = tar.getmembers()
names = [m.name for m in members]
top = names[0]
assert top.startswith("stack-agent-" + version + "-") and "/" not in top, names
rest = names[1:]
assert rest == sorted(rest), rest
assert set(rest) == {top + "/" + n for n in ("LICENSE", "LICENSE-APACHE", "README.md", "RELEASE_NOTES.md", "manifest.json", "stack-agent")}, rest
for m in members:
    assert (m.uid, m.gid, m.uname, m.gname) == (0, 0, "", ""), m.name
    assert m.mtime == 1700000000, (m.name, m.mtime)
    assert m.mode in (0o755, 0o644), (m.name, oct(m.mode))
assert next(m for m in members if m.name == top + "/stack-agent").mode == 0o755
PY
pass "entries are sorted, owned by no one, with fixed modes and times"

# 4. The manifest.
python3 -I - "$a" "$version" <<'PY' || fail "the manifest is wrong"
import json, sys, tarfile
path, version = sys.argv[1], sys.argv[2]
with tarfile.open(path) as tar:
    top = tar.getnames()[0]
    m = json.load(tar.extractfile(top + "/manifest.json"))
assert m["version"] == version and top.endswith(m["target"]), m
assert m["interfaces"]["diagnostics"]["command"] == "doctor"
assert m["signing"] == "none"
PY
pass "the manifest names the version and target and says the executable is unsigned"

# 5. A tag that does not match the crate version stops packaging before anything is written.
if RELEASE_TAG=v99.0.0 sh scripts/package-release.sh "$work/c" >/dev/null 2>"$work/err"; then
  fail "a mismatched tag was accepted"
fi
grep -q 'does not match' "$work/err" || fail "no explanation for the mismatched tag: $(cat "$work/err")"
[ -z "$(ls "$work/c" 2>/dev/null)" ] || fail "something was written despite the mismatched tag"
RELEASE_TAG="v$version" sh scripts/package-release.sh "$work/d" >/dev/null || fail "the matching tag was refused"
pass "a release tag must equal v<crate version>"

# 6. A non-executable input is refused.
if STACK_AGENT_BINARY=$work/nonexistent sh scripts/package-release.sh "$work/e" >/dev/null 2>&1; then
  fail "a missing executable was accepted"
fi
pass "a missing executable is refused"

echo "test-package: $passed checks passed"
