#!/bin/sh
# Wiring tests for scripts/verify-provenance.sh, with a stand-in `gh`.
#
#   scripts/test-provenance.sh
#
# WHAT THIS DOES NOT SHOW. The stand-in is a few lines of shell that compares strings. Nothing here
# proves that GitHub's tooling rejects a modified artifact, a wrong repository, a wrong workflow or a
# wrong tag: only a real attestation checked by the real `gh attestation verify` can, and that is what
# the `provenance-check` job in .github/workflows/release.yml does on GitHub.
#
# WHAT IT DOES SHOW. verify-provenance.sh
#   - refuses to run without all three identity pins, and hands every pin to gh for every file;
#   - fails closed: gh failing, gh returning nothing, or an attested digest that is not the file's own
#     digest is a failure; a missing file is a failure;
#   - waits for a just-created attestation only as long as it is told to, and then fails;
#   - in `negative` mode, fails when a verifier ACCEPTS a case it must reject (a stand-in that accepts
#     everything), fails when the positive control fails, and passes when every case is rejected.
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
script=$root/scripts/verify-provenance.sh
work=$(mktemp -d "${TMPDIR:-/tmp}/stack-agent-test-provenance.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
passed=0
out=
fail() { echo "FAIL: $*" >&2; [ -z "$out" ] || printf '%s\n' "--- output ---" "$out" >&2; exit 1; }
pass() { passed=$((passed + 1)); echo "ok   $*"; }

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

REPO=owner/repo
WORKFLOW=owner/repo/.github/workflows/release.yml
REF=refs/heads/branch

printf 'genuine release archive\n' > "$work/a.tar.gz"
printf 'a different release archive\n' > "$work/b.tar.gz"
A=$(sha256_of "$work/a.tar.gz")

# The stand-in gh. STUB_MODE decides what it does; STUB_LOG records every call.
cat > "$work/gh" <<'EOF'
#!/bin/sh
echo "$*" >> "$STUB_LOG"
[ "$1" = attestation ] || exit 2
sub=$2
shift 2
case $sub in
  download)
    # Like gh: the artifact must exist where gh runs, or it fails (this is what a relative path breaks).
    [ -f "$1" ] || { echo "failed to open local artifact: $1" >&2; exit 1; }
    # Writes a one-line bundle named after the digest, as gh does.
    d=$(sha256sum "$1" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$1" | cut -d' ' -f1)
    printf '{"dsseEnvelope":{"signatures":[{"sig":"QUJD"}]}}\n' > "sha256:$d.jsonl"
    exit 0 ;;
  verify) ;;
  *) exit 2 ;;
esac
file=$1; shift
[ -f "$file" ] || { echo "failed to open local artifact: $file" >&2; exit 1; }
bundle= repo= workflow= ref=
while [ $# -gt 0 ]; do
  case $1 in
    --bundle) bundle=$2; shift 2 ;;
    --repo) repo=$2; shift 2 ;;
    --signer-workflow) workflow=$2; shift 2 ;;
    --source-ref) ref=$2; shift 2 ;;
    *) shift ;;
  esac
done
digest=$(sha256sum "$file" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$file" | cut -d' ' -f1)
json() { printf '[{"verificationResult":{"statement":{"subject":[{"digest":{"sha256":"%s"}}]},"signature":{"certificate":{"githubWorkflowRef":"%s"}}}}]\n' "$1" "$2"; }
case $STUB_MODE in
  fail) echo "verification failed" >&2; exit 1 ;;
  empty) echo '[]'; exit 0 ;;
  wrongdigest) json "$(printf '%064d' 7)" "$ref"; exit 0 ;;
  acceptall) json "$digest" "$ref"; exit 0 ;;
  flaky)
    n=$(cat "$STUB_COUNT" 2>/dev/null || echo 0); echo $((n + 1)) > "$STUB_COUNT"
    [ "$n" -ge "$STUB_FAIL_FIRST" ] || { echo "no attestations found yet" >&2; exit 1; }
    json "$digest" "$ref"; exit 0 ;;
  strict)
    # Accepts only the genuine identity, the genuine artifact and an intact bundle.
    [ "$repo" = owner/repo ] || { echo "expected repo owner/repo, got $repo" >&2; exit 1; }
    [ "$workflow" = owner/repo/.github/workflows/release.yml ] || { echo "unexpected signer workflow" >&2; exit 1; }
    [ "$ref" = refs/heads/branch ] || { echo "unexpected ref" >&2; exit 1; }
    [ "$digest" = "$STUB_ATTESTED" ] || { echo "no attestation for digest $digest" >&2; exit 1; }
    if [ -n "$bundle" ]; then grep -q '"sig": "QUJD"\|"sig":"QUJD"' "$bundle" 2>/dev/null || { echo "bundle signature invalid" >&2; exit 1; }; fi
    json "$digest" "$ref"; exit 0 ;;
esac
exit 3
EOF
chmod 755 "$work/gh"

run() {
  mode=$1
  shift
  : > "$work/log"
  rm -f "$work/count"
  # shellcheck disable=SC2086 # EXTRA_ENV is a list of NAME=value words
  if out=$(cd "$work" && env GH="$work/gh" STUB_MODE="$mode" STUB_LOG="$work/log" STUB_COUNT="$work/count" STUB_ATTESTED="$A" \
      ${EXTRA_ENV:-} sh "$script" "$@" 2>&1); then rc=0; else rc=$?; fi
  unset EXTRA_ENV
}

pins="--repo $REPO --signer-workflow $WORKFLOW --source-ref $REF"

# 1. A verification passes only with all three pins handed to gh for every file.
# shellcheck disable=SC2086
run acceptall verify $pins "$work/a.tar.gz" "$work/b.tar.gz"
[ "$rc" = 0 ] || fail "a valid verification failed"
[ "$(grep -c 'attestation verify' "$work/log")" = 2 ] || fail "gh was not asked about both files"
while read -r line; do
  case $line in
    *"--repo $REPO"*"--signer-workflow $WORKFLOW"*"--source-ref $REF"*"--format json"*) ;;
    *) fail "a call to gh lacked a pin: $line" ;;
  esac
done < "$work/log"
case $out in *"verified a.tar.gz: sha256 $A"*) ;; *) fail "the summary does not show the digest" ;; esac
pass "every file is verified with the repository, signer workflow and ref pinned in each call to gh"

# 2. Without all three pins nothing is verified and gh is never called.
for missing in "--signer-workflow $WORKFLOW --source-ref $REF" "--repo $REPO --source-ref $REF" "--repo $REPO --signer-workflow $WORKFLOW"; do
  # shellcheck disable=SC2086
  run acceptall verify $missing "$work/a.tar.gz"
  [ "$rc" != 0 ] || fail "accepted without all pins: $missing"
  [ ! -s "$work/log" ] || fail "gh was called without all pins"
done
pass "a missing repository, workflow or ref pin is refused before gh is called"

# 3. Fail closed.
# shellcheck disable=SC2086
run fail verify $pins "$work/a.tar.gz"
[ "$rc" != 0 ] || fail "gh failing was accepted"
case $out in *"NOT VERIFIED"*) ;; *) fail "no NOT VERIFIED message" ;; esac
# shellcheck disable=SC2086
run empty verify $pins "$work/a.tar.gz"
[ "$rc" != 0 ] || fail "an empty result from gh was accepted"
# shellcheck disable=SC2086
run wrongdigest verify $pins "$work/a.tar.gz"
[ "$rc" != 0 ] || fail "an attested digest that is not the file's was accepted"
case $out in *"do not include the file's sha256"*) ;; *) fail "the digest mismatch is not explained" ;; esac
# shellcheck disable=SC2086
run acceptall verify $pins "$work/does-not-exist"
[ "$rc" != 0 ] || fail "a missing file was accepted"
# shellcheck disable=SC2086
run acceptall verify $pins
[ "$rc" != 0 ] || fail "no files was accepted"
pass "gh failing, an empty result, a foreign digest, a missing file and no files all fail"

# 4. Waiting for a just-created attestation is bounded and then fails.
# shellcheck disable=SC2086
EXTRA_ENV="STUB_FAIL_FIRST=2 PROVENANCE_LOOKUP_ATTEMPTS=3 PROVENANCE_LOOKUP_DELAY=0" run flaky verify $pins "$work/a.tar.gz"
[ "$rc" = 0 ] || fail "a verification that succeeds on the third attempt was refused"
# shellcheck disable=SC2086
EXTRA_ENV="STUB_FAIL_FIRST=5 PROVENANCE_LOOKUP_ATTEMPTS=3 PROVENANCE_LOOKUP_DELAY=0" run flaky verify $pins "$work/a.tar.gz"
[ "$rc" != 0 ] || fail "a verification that never succeeds was accepted"
[ "$(grep -c 'attestation verify' "$work/log")" = 3 ] || fail "more or fewer than 3 attempts were made"
pass "the wait for a new attestation is bounded by PROVENANCE_LOOKUP_ATTEMPTS and then fails"

# 5. The negative harness.
# shellcheck disable=SC2086
run acceptall negative $pins --artifact "$work/a.tar.gz" --other "$work/b.tar.gz"
[ "$rc" != 0 ] || fail "the negative harness passed against a verifier that accepts everything"
case $out in *"ACCEPTED (this is a failure)"*) ;; *) fail "an accepted case is not reported" ;; esac
# shellcheck disable=SC2086
run fail negative $pins --artifact "$work/a.tar.gz" --other "$work/b.tar.gz"
[ "$rc" != 0 ] || fail "the negative harness passed although the positive control failed"
case $out in *"control failed"*) ;; *) fail "a failed control is not reported" ;; esac
# Relative paths, as the workflow passes them (dist/...): the bundle is downloaded from another directory.
# shellcheck disable=SC2086
run strict negative $pins --artifact a.tar.gz --other b.tar.gz
[ "$rc" = 0 ] || fail "the negative harness failed against a strict stand-in verifier given relative paths"
for case_name in "modified artifact" "digest mismatch" "unexpected repository" "unexpected signer workflow" "unexpected git ref" "never attested" "corrupted signature" "invalid bundle"; do
  case $out in *"rejected: "*"$case_name"*) ;; *) fail "case '$case_name' was not run and rejected" ;; esac
done
pass "the negative harness fails on an accepting verifier or a failed control, and passes when all 8 kinds of case are rejected (stand-in)"

echo "test-provenance: $passed checks passed (wiring only; cryptographic rejection is shown by the GitHub run)"
