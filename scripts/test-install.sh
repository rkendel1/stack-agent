#!/bin/sh
# Tests for scripts/install.sh against a local release directory. No network, no real executable.
#
#   scripts/test-install.sh
#
# The "release" is made by the real scripts/package-release.sh from a stand-in executable, so the
# archive format the installer reads is the one the packager writes. Hostile archives are forged
# with Python. A restricted tool directory is the whole PATH of every run, so a tool that is meant to
# be absent (`gh`) is absent whatever this machine has installed.
#
# What this does not show: a download over the network from GitHub, GitHub's redirects, or a real
# attestation (a stand-in `gh` answers). Those need a published release.
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/stack-agent-test-install.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
installer=$root/scripts/install.sh
passed=0
fail() { echo "FAIL: $*" >&2; [ -z "${out:-}" ] || printf '%s\n' "--- output ---" "$out" >&2; exit 1; }
pass() { passed=$((passed + 1)); echo "ok   $*"; }

target=$(sh "$installer" --print-target)
# The platform the installer will see when `uname` is stood in for: this host's own, so the fake
# release (built for this host) is the archive it asks for.
case $target in
  aarch64-apple-darwin) host_os=Darwin host_arch=arm64 ;;
  x86_64-unknown-linux-gnu) host_os=Linux host_arch=x86_64 ;;
  *) echo "test-install: unexpected host target $target" >&2; exit 1 ;;
esac

# The tools the installer may use, and nothing else.
tools=$work/tools
mkdir "$tools"
for t in sh curl tar gzip awk sed grep cut tr head wc mktemp cat rm mkdir cp chmod mv ln ls dirname sort find cksum od dd uname basename getconf; do
  p=$(command -v "$t") || fail "this machine lacks $t"
  ln -s "$p" "$tools/$t"
done
if command -v sha256sum >/dev/null 2>&1; then ln -s "$(command -v sha256sum)" "$tools/sha256sum"; else ln -s "$(command -v shasum)" "$tools/shasum"; fi

# ---- a genuine release made by the packager from a stand-in executable --------------------------
cd "$root"
version=$(cargo metadata --format-version 1 --no-deps --locked | python3 -I -c '
import json, sys
print({p["name"]: p["version"] for p in json.load(sys.stdin)["packages"]}["stack-agent"])')
bin=$work/stack-agent
printf '#!/bin/sh\necho "stack-agent %s"\n' "$version" > "$bin"
chmod 755 "$bin"
release=$work/release
STACK_AGENT_BINARY=$bin SOURCE_DATE_EPOCH=1700000000 sh scripts/package-release.sh "$release" >/dev/null
cat "$release"/*.sha256 > "$release/SHA256SUMS"
archive=$(find "$release" -name '*.tar.gz' -exec basename {} \;)

home=$work/home
logbin=$work/curl.log

reset_home() {
  rm -rf "$home" "$work/tmp"
  mkdir -p "$home/.ssh" "$home/project" "$home/.local/share/stack-agent" "$work/tmp"
  echo "private" > "$home/.ssh/id_test"
  echo "OPENAI_API_KEY=sk-test" > "$home/.env"
  echo "export X=1" > "$home/.zshrc"
  echo "export Y=2" > "$home/.bashrc"
  echo "export Z=3" > "$home/.profile"
  echo "[package]" > "$home/project/Cargo.toml"
  echo "state" > "$home/.local/share/stack-agent/state.db"
}

# Everything under HOME except the install directory: paths and content checksums.
snap() {
  find "$home" -path "$home/.local/bin" -prune -o -print | sort
  find "$home" -path "$home/.local/bin" -prune -o -type f -exec cksum {} + | sort
}

# Runs the installer with a clean environment. Sets $out (stdout and stderr) and $rc.
run() {
  base=$1
  shift
  # shellcheck disable=SC2086 # RUN_ENV is a list of NAME=value words
  if out=$(env -i HOME="$home" TMPDIR="$work/tmp" PATH="${RUN_PATH:-$tools}" \
    STACK_AGENT_BASE_URL="$base" ${RUN_ENV:-} sh "$installer" "$@" 2>&1); then rc=0; else rc=$?; fi
  # In POSIX sh, `VAR=x function` leaves VAR set afterwards; a run's settings must not leak into the next.
  unset RUN_PATH RUN_ENV
}

installed() { [ -e "$home/.local/bin/stack-agent" ]; }

# The last run must have failed, and said something containing $1.
refused() {
  [ "$rc" != 0 ] || fail "$2"
  case $out in *"$1"*) ;; *) fail "$2 (the message does not say '$1')" ;; esac
}

# A forged release: a valid archive and a SHA256SUMS that lists it, with chosen defects.
#   forge DIR NAME_VERSION MANIFEST_TARGET MANIFEST_VERSION BINARY_VERSION EXTRA_MEMBER
forge() {
  python3 -I - "$@" "$target" <<'PY'
import gzip, hashlib, io, json, os, sys, tarfile
out, name_version, m_target, m_version, bin_version, extra, target = sys.argv[1:8]
os.makedirs(out, exist_ok=True)
top = f"stack-agent-{name_version}-{target}"
raw = io.BytesIO()
def add(tar, path, data, mode=0o644):
    info = tarfile.TarInfo(path)
    info.size, info.mode, info.mtime = len(data), mode, 1700000000
    tar.addfile(info, io.BytesIO(data))
with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
    d = tarfile.TarInfo(top); d.type = tarfile.DIRTYPE; d.mode = 0o755; d.mtime = 1700000000
    tar.addfile(d)
    if bin_version == "CRASH":
        script = '#!/bin/sh\necho "libc.so.6: version GLIBC_2.34 not found (required by stack-agent)" >&2\nexit 1\n'
    else:
        script = f'#!/bin/sh\necho "stack-agent {bin_version}"\n'
    add(tar, top + "/stack-agent", script.encode(), 0o755)
    add(tar, top + "/manifest.json", (json.dumps({"target": m_target, "version": m_version}, indent=2, sort_keys=True) + "\n").encode())
    if extra != "none":
        add(tar, extra, b"escaped\n")
data = gzip.compress(raw.getvalue(), mtime=0)
name = top + ".tar.gz"
open(os.path.join(out, name), "wb").write(data)
open(os.path.join(out, "SHA256SUMS"), "w").write(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
PY
}

# ---- 1. a clean install ------------------------------------------------------------------------
reset_home
before=$(snap)
run "file://$release"
[ "$rc" = 0 ] || fail "a clean install failed ($rc)"
installed || fail "nothing was installed"
[ "$("$home/.local/bin/stack-agent" --version)" = "stack-agent $version" ] || fail "the installed executable does not report $version"
case $out in *"checksum verified"*) ;; *) fail "no checksum confirmation" ;; esac
case $out in *"is not on your PATH"*'export PATH='*) ;; *) fail "no PATH hint when the directory is not on PATH" ;; esac
case $out in *"does not edit your shell profile"*) ;; *) fail "the PATH hint does not say profiles are untouched" ;; esac
[ "$(ls -A "$home/.local/bin")" = "stack-agent" ] || fail "extra files in the install directory: $(ls -A "$home/.local/bin")"
[ "$(snap)" = "$before" ] || fail "the install changed something under HOME other than the install directory"
pass "a clean install verifies, installs one executable, hints at PATH, and changes nothing else under HOME"

# ---- 2. an existing executable is not replaced without --force ----------------------------------
echo "original" > "$home/.local/bin/stack-agent"
run "file://$release"
[ "$rc" != 0 ] || fail "an existing executable was overwritten without --force"
[ "$(cat "$home/.local/bin/stack-agent")" = "original" ] || fail "the existing file was modified"
case $out in *"already exists"*"--force"*) ;; *) fail "the refusal does not explain --force" ;; esac
pass "an existing executable is refused without --force and left untouched"

# ---- 3. --force replaces it atomically, with no leftovers ---------------------------------------
run "file://$release" --force
[ "$rc" = 0 ] || fail "--force failed ($rc)"
[ "$("$home/.local/bin/stack-agent" --version)" = "stack-agent $version" ] || fail "--force did not replace the file"
[ "$(ls -A "$home/.local/bin")" = "stack-agent" ] || fail "temporary files were left: $(ls -A "$home/.local/bin")"
pass "--force replaces the executable and leaves no temporary file"

# ---- 4. a directory in the way is never replaced -----------------------------------------------
rm -f "$home/.local/bin/stack-agent"
mkdir "$home/.local/bin/stack-agent"
run "file://$release" --force
[ "$rc" != 0 ] || fail "a directory was replaced"
[ -d "$home/.local/bin/stack-agent" ] || fail "the directory is gone"
rmdir "$home/.local/bin/stack-agent"
pass "a directory at the destination is refused even with --force"

# ---- 5. a corrupt archive stops before extraction ----------------------------------------------
reset_home
corrupt=$work/corrupt
cp -R "$release" "$corrupt"
dd if=/dev/zero of="$corrupt/$archive" bs=1 count=16 seek=200 conv=notrunc 2>/dev/null
run "file://$corrupt"
[ "$rc" != 0 ] || fail "a corrupt archive was accepted"
case $out in *"checksum mismatch"*"Nothing was extracted or installed"*) ;; *) fail "no clear checksum-mismatch message" ;; esac
installed && fail "something was installed from a corrupt archive"
[ -z "$(ls -A "$work/tmp")" ] || fail "temporary files were left behind: $(ls -A "$work/tmp")"
pass "a corrupt archive is rejected before extraction and leaves no temporary files"

# ---- 6. a wrong digest in SHA256SUMS ------------------------------------------------------------
wrong=$work/wrong
cp -R "$release" "$wrong"
printf '%064d  %s\n' 0 "$archive" > "$wrong/SHA256SUMS"
run "file://$wrong"
refused "checksum mismatch" "a wrong listed digest was accepted"
installed && fail "something was installed despite a wrong digest"
pass "an archive that does not match the listed digest is rejected"

# ---- 7. an out-of-band digest that disagrees with the list -------------------------------------
RUN_ENV="STACK_AGENT_SHA256=$(printf '%064d' 1)" run "file://$release"
[ "$rc" != 0 ] || fail "a disagreeing STACK_AGENT_SHA256 was accepted"
case $out in *"STACK_AGENT_SHA256 does not match"*) ;; *) fail "no explanation for the disagreeing digest" ;; esac
installed && fail "something was installed"
good=$(cut -d' ' -f1 "$release/$archive.sha256")
RUN_ENV="STACK_AGENT_SHA256=$good" run "file://$release"
[ "$rc" = 0 ] || fail "a matching STACK_AGENT_SHA256 was refused"
pass "STACK_AGENT_SHA256 must agree with the list, and works when it does"

# ---- 8. an unsupported platform stops before any download ---------------------------------------
reset_home
stubs=$work/stubs
mkdir "$stubs"
# shellcheck disable=SC2016 # the stand-in's own variables must stay unexpanded
printf '#!/bin/sh\ncase "$1" in -s) echo "${FAKE_OS}" ;; -m) echo "${FAKE_ARCH}" ;; esac\n' > "$stubs/uname"
printf '#!/bin/sh\necho "$@" >> "%s"\nexit 1\n' "$logbin" > "$stubs/curl"
chmod 755 "$stubs/uname" "$stubs/curl"
: > "$logbin"
for platform in "Darwin x86_64" "Linux aarch64" "FreeBSD amd64" "Windows_NT x86_64"; do
  # shellcheck disable=SC2086 # "OS ARCH" is split on purpose
  set -- $platform
  RUN_PATH="$stubs:$tools" RUN_ENV="FAKE_OS=$1 FAKE_ARCH=$2" run "file://$release"
  [ "$rc" != 0 ] || fail "$platform was accepted"
  case $out in *"not supported"*"Nothing was downloaded"*) ;; *) fail "$platform: unclear message" ;; esac
done
[ ! -s "$logbin" ] || fail "curl was called for an unsupported platform: $(cat "$logbin")"
installed && fail "something was installed on an unsupported platform"
pass "macOS Intel, Linux arm64, FreeBSD and Windows are refused before any download"

# ---- 8b. the Linux glibc floor: refused before any download; an executable that cannot run is explained
: > "$logbin"
# shellcheck disable=SC2016 # the stand-in's own variable must stay unexpanded
printf '#!/bin/sh\necho "glibc ${FAKE_GLIBC}"\n' > "$stubs/getconf"
printf '#!/bin/sh\necho "$@" >> "%s"\nexit 1\n' "$logbin" > "$stubs/curl"
chmod 755 "$stubs/getconf" "$stubs/curl"
for old in 2.17 2.28 2.31 2.33; do
  RUN_PATH="$stubs:$tools" RUN_ENV="FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_GLIBC=$old" run "file://$release"
  refused "needs glibc 2.34 or newer" "glibc $old was accepted"
  case $out in *"has glibc $old"*"Nothing was downloaded"*) ;; *) fail "glibc $old: unclear message" ;; esac
done
[ ! -s "$logbin" ] || fail "curl was called for a glibc older than the floor"
rm -f "$stubs/curl"
for ok in 2.34 2.35 2.39 3.0; do
  # Past the glibc gate the fake release (built for this host) lists no archive for the pretended
  # platform, which is the next thing that can fail; failing there shows the gate let it through.
  RUN_PATH="$stubs:$tools" RUN_ENV="FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_GLIBC=$ok" run "file://$release"
  case $out in *"has glibc"*) fail "glibc $ok was refused by the floor" ;; esac
done
rm -f "$stubs/getconf"
pass "Linux with glibc older than 2.34 is refused before any download; 2.34 and newer pass the gate"

reset_home
forge "$work/crash" "$version" "$target" "$version" "CRASH" none
run "file://$work/crash"
refused "does not run on this machine" "an executable that cannot run was installed"
case $out in *"GLIBC_2.34"*) ;; *) fail "the loader's own error is not shown" ;; esac
installed && fail "something was installed"
pass "an executable that cannot run is not installed, and the loader's error is shown"

# ---- 9. hostile archives ------------------------------------------------------------------------
reset_home
forge "$work/evil" "$version" "$target" "$version" "$version" "../evil"
run "file://$work/evil"
refused "unexpected path" "an archive with a '..' member was accepted"
installed && fail "something was installed from a traversal archive"
forge "$work/abs" "$version" "$target" "$version" "$version" "/tmp/evil"
run "file://$work/abs"
refused "unexpected path" "an archive with an absolute member was accepted"
pass "archives with '..' or absolute members are rejected before extraction"

forge "$work/wrongtarget" "$version" "x86_64-unknown-linux-musl" "$version" "$version" none
run "file://$work/wrongtarget"
refused "manifest is for" "a manifest for another target was accepted"
forge "$work/wrongversion" "$version" "$target" "$version" "9.9.9" none
run "file://$work/wrongversion"
refused "reports 'stack-agent 9.9.9'" "an executable that reports another version was accepted"
installed && fail "something was installed"
pass "a manifest for another target, or an executable that reports another version, is rejected"

# ---- 10. --version selects a release and is validated ------------------------------------------
run "file://$release" --version "$version" --dry-run
[ "$rc" = 0 ] || fail "--dry-run with a version failed"
run "file://$release" --version "1.0; rm -rf ~"
[ "$rc" != 0 ] || fail "a hostile --version was accepted"
run "file://$release" --version "99.0.0"
[ "$rc" != 0 ] || fail "a version the release does not list was installed"
installed && fail "something was installed"
pass "--version is validated and must match what the release lists"

# ---- 11. --dry-run downloads and writes nothing -------------------------------------------------
reset_home
: > "$logbin"
before=$(snap)
RUN_PATH="$stubs:$tools" RUN_ENV="FAKE_OS=$host_os FAKE_ARCH=$host_arch" run "file://$release" --dry-run
[ "$rc" = 0 ] || fail "--dry-run failed"
[ ! -s "$logbin" ] || fail "--dry-run called curl"
[ ! -e "$home/.local/bin" ] || fail "--dry-run created the install directory"
[ "$(snap)" = "$before" ] || fail "--dry-run changed HOME"
pass "--dry-run downloads and writes nothing"

# ---- 12. the install directory on PATH needs no hint; credentials and profiles untouched -------
reset_home
before=$(snap)
RUN_PATH="$home/.local/bin:$tools" run "file://$release"
[ "$rc" = 0 ] || fail "install with the directory on PATH failed"
case $out in *"is not on your PATH"*) fail "a PATH hint was printed although the directory is on PATH" ;; esac
[ "$(snap)" = "$before" ] || fail "credentials, profiles or state changed"
[ "$(cat "$home/.zshrc")" = "export X=1" ] || fail "a shell profile was edited (.zshrc)"
[ "$(cat "$home/.bashrc")" = "export Y=2" ] || fail "a shell profile was edited (.bashrc)"
[ "$(cat "$home/.profile")" = "export Z=3" ] || fail "a shell profile was edited (.profile)"
pass "no hint when the directory is on PATH; profiles, credentials, project and state are untouched"

# ---- 13. a custom install directory, with no write outside it ----------------------------------
reset_home
RUN_ENV="STACK_AGENT_INSTALL_DIR=$work/custom/bin" run "file://$release"
[ "$rc" = 0 ] || fail "install into a custom directory failed"
[ -x "$work/custom/bin/stack-agent" ] || fail "nothing was installed in the custom directory"
[ ! -e "$home/.local/bin" ] || fail "the default directory was used as well"
RUN_ENV="STACK_AGENT_INSTALL_DIR=relative/bin" run "file://$release"
[ "$rc" != 0 ] || fail "a relative install directory was accepted"
pass "STACK_AGENT_INSTALL_DIR is honoured and must be absolute"

# ---- 14. attestation verification --------------------------------------------------------------
reset_home
rm -f "$stubs/curl" # the failing stand-in from the platform case; this one needs the real curl
printf '#!/bin/sh\necho "$@" >> "%s.gh"\nexit 0\n' "$work/att" > "$stubs/gh"
chmod 755 "$stubs/gh"
RUN_PATH="$stubs:$tools" RUN_ENV="FAKE_OS=$host_os FAKE_ARCH=$host_arch" run "file://$release" --verify-attestation
[ "$rc" = 0 ] || fail "install with a passing attestation check failed"
grep -q 'attestation verify' "$work/att.gh" || fail "gh was not asked to verify an attestation"
grep -q -- '--repo rkendel1/stack-agent' "$work/att.gh" || fail "gh was not asked about this repository"
grep -q -- '--signer-workflow rkendel1/stack-agent/.github/workflows/release.yml' "$work/att.gh" || fail "gh was not asked to pin the signer workflow"
grep -q -- "--source-ref refs/tags/v$version" "$work/att.gh" || fail "gh was not asked to pin the release tag v$version"
installed || fail "not installed after a passing attestation"
reset_home
printf '#!/bin/sh\nexit 1\n' > "$stubs/gh"
RUN_PATH="$stubs:$tools" RUN_ENV="FAKE_OS=$host_os FAKE_ARCH=$host_arch" run "file://$release" --verify-attestation
[ "$rc" != 0 ] || fail "a failed attestation was accepted"
installed && fail "installed despite a failed attestation"
rm -f "$stubs/gh"
run "file://$release" --verify-attestation
refused "needs the GitHub CLI" "a missing gh was not reported"
pass "--verify-attestation requires gh, asks it about this repository, and installs nothing on failure (gh is a stand-in here)"

# ---- 15. static properties ---------------------------------------------------------------------
# Only executable lines: comments and the usage text (the heredoc) may talk about what it never does.
code=$(awk '/<<.EOF.$/ { skip = 1; next } skip && /^EOF$/ { skip = 0; next } !skip && !/^[[:space:]]*#/' "$installer")
if printf '%s\n' "$code" | grep -nE '(^|[^[:alnum:]_])sudo([^[:alnum:]_]|$)|\.bashrc|\.zshrc|\.profile|\.zprofile|brew install|apt(-get)? install|pip install'; then
  fail "the installer's code mentions sudo, a shell profile or a package manager"
fi
pass "the installer's code never mentions sudo, shell profiles or a package manager"

echo "test-install: $passed checks passed"
