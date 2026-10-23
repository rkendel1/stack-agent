#!/bin/sh
# Install stack-agent from a GitHub release into ~/.local/bin. No administrator rights.
#
#   curl -fsSL https://github.com/rkendel1/stack-agent/releases/latest/download/install.sh | sh
#   sh install.sh [--version X.Y.Z] [--force] [--dry-run] [--verify-attestation] [--print-target]
#
# What it does, in order:
#   1. works out the platform and stops *before any download* if it is not supported;
#   2. fetches SHA256SUMS and the archive for this platform from one release;
#   3. verifies the archive's SHA-256 *before extracting anything*;
#   4. checks the archive's member names, extracts it into a private temporary directory, and checks
#      that the manifest and `stack-agent --version` agree with the platform and the release;
#   5. installs the executable, refusing to replace an existing file unless --force is given.
#
# What it never does: use sudo, edit a shell profile, install a dependency, read your project or
# your credentials, run `doctor` or `setup`, or touch stack-agent's durable state. It installs one
# file. To uninstall, delete it; state stays where `stack-agent doctor` reports it.
#
# What the checksum means: SHA256SUMS comes from the same release as the archive, so verifying it
# shows the archive arrived intact and is the one the release lists. It does not show who published
# the release. For that, pass --verify-attestation (needs the GitHub CLI, `gh`), which checks the
# archive's build-provenance attestation, or set STACK_AGENT_SHA256 to a digest you obtained
# elsewhere. The executable is not code-signed or notarized.
#
# Environment: STACK_AGENT_INSTALL_DIR (default $HOME/.local/bin), STACK_AGENT_SHA256 (a digest you
# trust, which must also match SHA256SUMS), STACK_AGENT_BASE_URL (a different release location, for
# mirrors and for the installer's own tests; it is printed when used).
set -eu

REPO=rkendel1/stack-agent
# The oldest glibc the Linux release runs on. Measured, not assumed: the executable imports GLIBC_2.34
# symbols and fails on glibc 2.31 (Debian 11, Ubuntu 20.04). release.yml declares the same floor and
# fails the build if the executable ever needs more; scripts/check-release-workflow.py keeps the two equal.
GLIBC_FLOOR=2.34
SUPPORTED="macOS on Apple Silicon (arm64) and Linux x86_64 with glibc $GLIBC_FLOOR or newer"
VERSION=
FORCE=0
DRY_RUN=0
ATTEST=0
PRINT_TARGET=0

usage() {
  cat <<'EOF'
Install stack-agent from a GitHub release into ~/.local/bin. No administrator rights.

  curl -fsSL https://github.com/rkendel1/stack-agent/releases/latest/download/install.sh | sh
  sh install.sh [--version X.Y.Z] [--force] [--dry-run] [--verify-attestation] [--print-target]

It stops before any download on an unsupported platform, verifies the archive's SHA-256 before
extracting anything, refuses to replace an existing stack-agent unless --force is given, and never
uses sudo, edits a shell profile, installs a dependency or reads your project or credentials.
The checksum shows integrity, not who published the release: use --verify-attestation (needs gh)
or STACK_AGENT_SHA256 for that. The executable is not code-signed or notarized.
Environment: STACK_AGENT_INSTALL_DIR, STACK_AGENT_SHA256, STACK_AGENT_BASE_URL.
EOF
}

die() {
  echo "install: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case $1 in
    --version)
      [ $# -ge 2 ] || die "--version needs a value such as 0.1.0"
      VERSION=$2
      shift
      ;;
    --force) FORCE=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --verify-attestation) ATTEST=1 ;;
    --print-target) PRINT_TARGET=1 ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
  shift
done

if [ -n "$VERSION" ]; then
  case $VERSION in
    *[!0-9A-Za-z.-]* | '' | -* | .*) die "--version must look like 0.1.0 or 0.1.0-rc1, not '$VERSION'" ;;
  esac
  printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' ||
    die "--version must look like 0.1.0 or 0.1.0-rc1, not '$VERSION'"
fi

# 1. The platform. Nothing has been downloaded and nothing has been written.
os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Darwin/arm64) TARGET=aarch64-apple-darwin ;;
  Linux/x86_64)
    TARGET=x86_64-unknown-linux-gnu
    if ls /lib/ld-musl-* /usr/lib/ld-musl-* >/dev/null 2>&1; then
      die "this Linux uses musl libc; the release is built for glibc ($TARGET). Supported: $SUPPORTED. Nothing was downloaded."
    fi
    # `getconf` prints "glibc 2.35". If it does not say, the check after the download still catches
    # an executable that cannot run.
    glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')
    if [ -n "$glibc" ] && ! awk -v have="$glibc" -v want="$GLIBC_FLOOR" 'BEGIN {
      split(have, h, "."); split(want, w, ".")
      exit !((h[1] + 0 > w[1] + 0) || (h[1] + 0 == w[1] + 0 && h[2] + 0 >= w[2] + 0))
    }'; then
      die "this Linux has glibc $glibc; the release needs glibc $GLIBC_FLOOR or newer (Ubuntu 22.04, Debian 12 or newer). Supported: $SUPPORTED. Nothing was downloaded."
    fi
    ;;
  *)
    die "$os $arch is not supported. Supported: $SUPPORTED. Nothing was downloaded."
    ;;
esac

if [ "$PRINT_TARGET" = 1 ]; then
  echo "$TARGET"
  exit 0
fi

# What is needed to continue, checked before the first download.
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"
command -v gzip >/dev/null 2>&1 || die "gzip is required"
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "sha256sum or shasum is required to verify the download"
fi
if [ "$ATTEST" = 1 ]; then
  command -v gh >/dev/null 2>&1 || die "--verify-attestation needs the GitHub CLI (gh), which is not installed"
fi

# Where it goes.
if [ -n "${STACK_AGENT_INSTALL_DIR:-}" ]; then
  INSTALL_DIR=$STACK_AGENT_INSTALL_DIR
else
  [ -n "${HOME:-}" ] || die "HOME is not set; set STACK_AGENT_INSTALL_DIR"
  INSTALL_DIR=$HOME/.local/bin
fi
case $INSTALL_DIR in
  /*) ;;
  *) die "the install directory must be an absolute path, not '$INSTALL_DIR'" ;;
esac
DEST=$INSTALL_DIR/stack-agent

# Where from.
if [ -n "${STACK_AGENT_BASE_URL:-}" ]; then
  BASE=${STACK_AGENT_BASE_URL%/}
  echo "install: using a non-default release location: $BASE" >&2
  CURL_SECURITY=
elif [ -n "$VERSION" ]; then
  BASE=https://github.com/$REPO/releases/download/v$VERSION
  CURL_SECURITY="--proto =https --proto-redir =https --tlsv1.2"
else
  BASE=https://github.com/$REPO/releases/latest/download
  CURL_SECURITY="--proto =https --proto-redir =https --tlsv1.2"
fi

# An existing file is never replaced without --force, and a directory never.
if [ -d "$DEST" ] && [ ! -L "$DEST" ]; then
  die "$DEST is a directory; nothing was changed"
fi
if { [ -e "$DEST" ] || [ -L "$DEST" ]; } && [ "$FORCE" != 1 ]; then
  die "$DEST already exists; nothing was changed. Run again with --force to replace it, or remove it yourself."
fi

if [ "$DRY_RUN" = 1 ]; then
  echo "install (dry run): nothing is downloaded or written."
  echo "  platform:    $TARGET"
  echo "  release:     $BASE"
  echo "  install to:  $DEST$([ "$FORCE" = 1 ] && [ -e "$DEST" ] && echo ' (would replace the existing file)')"
  echo "  verify:      SHA-256 against SHA256SUMS$([ -n "${STACK_AGENT_SHA256:-}" ] && echo ' and STACK_AGENT_SHA256')$([ "$ATTEST" = 1 ] && echo ', then the build-provenance attestation')"
  exit 0
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/stack-agent-install.XXXXXX")
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT HUP INT TERM

fetch() {
  # shellcheck disable=SC2086 # CURL_SECURITY is a list of flags
  curl -fsSL $CURL_SECURITY -o "$2" "$1" || die "could not download $1"
}

# 2. The checksum list, and from it the one archive for this platform.
fetch "$BASE/SHA256SUMS" "$tmp/SHA256SUMS"
if [ -n "$VERSION" ]; then
  want="stack-agent-$VERSION-$TARGET.tar.gz"
else
  want=
fi
matches=$(awk -v target="$TARGET" -v want="$want" '
  NF == 2 && $2 ~ ("^stack-agent-[0-9A-Za-z.+-]+-" target "\\.tar\\.gz$") && (want == "" || $2 == want) { print $1 " " $2 }
' "$tmp/SHA256SUMS")
[ -n "$matches" ] || die "SHA256SUMS lists no archive for $TARGET${VERSION:+ at version $VERSION}; nothing was installed"
[ "$(printf '%s\n' "$matches" | wc -l | tr -d ' ')" = 1 ] || die "SHA256SUMS lists more than one archive for $TARGET; nothing was installed"
listed=${matches%% *}
archive_name=${matches#* }
printf '%s' "$listed" | grep -Eq '^[0-9a-f]{64}$' || die "SHA256SUMS has a malformed digest for $archive_name"

expected=$listed
if [ -n "${STACK_AGENT_SHA256:-}" ]; then
  [ "$STACK_AGENT_SHA256" = "$listed" ] ||
    die "STACK_AGENT_SHA256 does not match the digest SHA256SUMS lists for $archive_name; nothing was installed"
  expected=$STACK_AGENT_SHA256
fi

# 3. The archive, verified before anything is extracted.
fetch "$BASE/$archive_name" "$tmp/$archive_name"
actual=$(sha256_of "$tmp/$archive_name")
if [ "$actual" != "$expected" ]; then
  die "checksum mismatch for $archive_name (expected $expected, got $actual). Nothing was extracted or installed."
fi
echo "install: checksum verified for $archive_name"

if [ "$ATTEST" = 1 ]; then
  # Pinned to this repository, its release workflow and the release tag for the version in the archive's
  # name: an attestation made on a branch, by another workflow or for another tag is refused.
  att_version=${archive_name#stack-agent-}
  att_version=${att_version%-"$TARGET".tar.gz}
  gh attestation verify "$tmp/$archive_name" --repo "$REPO" \
    --signer-workflow "$REPO/.github/workflows/release.yml" --source-ref "refs/tags/v$att_version" >/dev/null ||
    die "the build-provenance attestation for $archive_name did not verify against $REPO, its release workflow and tag v$att_version; nothing was installed"
  echo "install: build-provenance attestation verified"
fi

# 4. The archive's contents: every member under one top-level directory, no absolute or '..' path.
top=${archive_name%.tar.gz}
members=$(tar -tzf "$tmp/$archive_name") || die "$archive_name is not a readable archive"
bad=$(printf '%s\n' "$members" | awk -v top="$top" '
  $0 == top || $0 == top "/" { next }
  index($0, top "/") == 1 && $0 !~ /(^|\/)\.\.(\/|$)/ { next }
  { print; exit }')
[ -z "$bad" ] || die "$archive_name contains an unexpected path ($bad); nothing was extracted"

mkdir "$tmp/extract"
tar -xzf "$tmp/$archive_name" -C "$tmp/extract"
dir=$tmp/extract/$top
if [ ! -f "$dir/stack-agent" ] || [ ! -f "$dir/manifest.json" ]; then
  die "$archive_name lacks stack-agent or manifest.json"
fi
[ ! -L "$dir/stack-agent" ] || die "the executable in $archive_name is a symbolic link"

field() { sed -n "s/^  \"$1\": \"\\([^\"]*\\)\",\\{0,1\\}\$/\\1/p" "$dir/manifest.json" | head -1; }
m_target=$(field target)
m_version=$(field version)
[ "$m_target" = "$TARGET" ] || die "the manifest is for '$m_target', not $TARGET; nothing was installed"
[ -n "$m_version" ] || die "the manifest has no version"
[ -z "$VERSION" ] || [ "$m_version" = "$VERSION" ] || die "the manifest is version $m_version, not $VERSION; nothing was installed"
case $archive_name in
  "stack-agent-$m_version-$TARGET.tar.gz") ;;
  *) die "the archive name does not match its manifest (version $m_version); nothing was installed" ;;
esac
chmod 755 "$dir/stack-agent"
if ! reported=$("$dir/stack-agent" --version 2>"$tmp/run.err"); then
  die "the downloaded executable does not run on this machine: $(head -n 3 "$tmp/run.err" | cut -c1-200); nothing was installed"
fi
[ "$reported" = "stack-agent $m_version" ] ||
  die "the executable reports '$reported' but the manifest says $m_version; nothing was installed"

# 5. Install: one file, next to its destination first so the final step is a rename.
mkdir -p "$INSTALL_DIR"
staged=$INSTALL_DIR/.stack-agent.new.$$
cp "$dir/stack-agent" "$staged"
chmod 755 "$staged"
if [ "$FORCE" = 1 ]; then
  mv -f "$staged" "$DEST" || { rm -f "$staged"; die "could not replace $DEST"; }
else
  # A hard link fails if the name exists, so a file created since the check above is not clobbered.
  if ln "$staged" "$DEST" 2>/dev/null; then
    rm -f "$staged"
  else
    rm -f "$staged"
    die "$DEST appeared while installing; nothing was changed. Run again with --force to replace it."
  fi
fi

echo "install: installed stack-agent $m_version to $DEST"
case ":${PATH:-}:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo
    echo "$INSTALL_DIR is not on your PATH. To use stack-agent from any directory, add it yourself, for example:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    echo "(this installer does not edit your shell profile)"
    ;;
esac
found=$(command -v stack-agent 2>/dev/null || true)
if [ -n "$found" ] && [ "$found" != "$DEST" ]; then
  echo
  echo "note: 'stack-agent' currently resolves to $found, not the one just installed."
fi
echo
echo "Next:"
echo "  stack-agent --version"
echo "  stack-agent doctor        # checks the model setup, PAX, git and the project; changes nothing"
