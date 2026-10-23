#!/bin/sh
# Verify GitHub build-provenance attestations for release files with `gh attestation verify`, pinned
# to an expected repository, signer workflow and git ref. No cryptography is implemented here: GitHub's
# tool checks the signature, the certificate chain and the transparency log. This script adds the
# pinning, the fail-closed handling, an explicit comparison of the attested digest with the file's own
# digest, and (for `negative`) controlled attempts that must be rejected.
#
#   verify-provenance.sh verify   --repo OWNER/REPO --signer-workflow OWNER/REPO/PATH --source-ref REF FILE...
#   verify-provenance.sh negative --repo OWNER/REPO --signer-workflow OWNER/REPO/PATH --source-ref REF \
#                                 --artifact FILE --other FILE
#
# `verify` exits 0 only if every FILE has a verified attestation whose subject digest equals the file's
# SHA-256 and whose signer workflow and ref match. Anything else, including `gh` failing, an empty
# result or a missing tool, exits non-zero.
#
# `negative` takes one attested FILE (--artifact) and one other file (--other) and requires:
#   - a positive control to pass first (so a failure below is not the environment);
#   - each of these to be REJECTED by gh, and says why: a modified copy of the artifact, the right
#     bundle for a different artifact (digest), a different repository, a different signer workflow, a
#     different git ref (a release-tag identity for a branch attestation), a file that was never
#     attested, and a bundle with a corrupted signature.
# A case that is accepted, or a control that fails, is a failure of the whole command.
#
# Environment: GH_TOKEN (needed to look attestations up), PROVENANCE_LOOKUP_ATTEMPTS (default 1; an
# attestation just created may take a moment to be queryable: attempts are bounded, logged, and the
# last failure is reported), PROVENANCE_LOOKUP_DELAY (seconds between attempts, default 10), GH
# (the gh executable, default `gh`; the wiring tests substitute a stand-in).
set -eu

GH=${GH:-gh}
ATTEMPTS=${PROVENANCE_LOOKUP_ATTEMPTS:-1}
DELAY=${PROVENANCE_LOOKUP_DELAY:-10}

die() {
  echo "verify-provenance: $*" >&2
  exit 1
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

command -v python3 >/dev/null 2>&1 || die "python3 is required to read gh's JSON output"

mode=${1:-}
[ -n "$mode" ] || die "usage: verify-provenance.sh verify|negative --repo R --signer-workflow W --source-ref REF ..."
shift

repo='' workflow='' ref='' artifact='' other=''
while [ $# -gt 0 ]; do
  case $1 in
    --repo) repo=${2:?--repo needs a value}; shift 2 ;;
    --signer-workflow) workflow=${2:?--signer-workflow needs a value}; shift 2 ;;
    --source-ref) ref=${2:?--source-ref needs a value}; shift 2 ;;
    --artifact) artifact=${2:?--artifact needs a file}; shift 2 ;;
    --other) other=${2:?--other needs a file}; shift 2 ;;
    --) shift; break ;;
    -*) die "unknown option $1" ;;
    *) break ;;
  esac
done
if [ -z "$repo" ] || [ -z "$workflow" ] || [ -z "$ref" ]; then
  die "--repo, --signer-workflow and --source-ref are all required: nothing is verified against an unpinned identity"
fi

# verify_one FILE [BUNDLE] [REPO WORKFLOW REF]  -> prints a summary on stdout, gh's stderr on stderr.
# Returns gh's status, or 1 if gh succeeded but the result is not what was asked for.
verify_one() {
  v_file=$1
  v_bundle=${2:-}
  v_repo=${3:-$repo}
  v_workflow=${4:-$workflow}
  v_ref=${5:-$ref}
  [ -f "$v_file" ] || { echo "no such file: $v_file" >&2; return 1; }
  v_out=$(mktemp "${TMPDIR:-/tmp}/provenance.XXXXXX")
  v_err=$(mktemp "${TMPDIR:-/tmp}/provenance.XXXXXX")
  v_n=1
  while :; do
    v_status=0
    if [ -n "$v_bundle" ]; then
      "$GH" attestation verify "$v_file" --bundle "$v_bundle" --repo "$v_repo" --signer-workflow "$v_workflow" \
        --source-ref "$v_ref" --format json >"$v_out" 2>"$v_err" || v_status=$?
    else
      "$GH" attestation verify "$v_file" --repo "$v_repo" --signer-workflow "$v_workflow" \
        --source-ref "$v_ref" --format json >"$v_out" 2>"$v_err" || v_status=$?
    fi
    [ "$v_status" -ne 0 ] || break
    if [ "$v_n" -ge "$ATTEMPTS" ]; then
      cat "$v_err" >&2
      rm -f "$v_out" "$v_err"
      return "$v_status"
    fi
    echo "attempt $v_n of $ATTEMPTS for $(basename "$v_file") did not verify; trying again in ${DELAY}s: $(head -n 1 "$v_err")" >&2
    v_n=$((v_n + 1))
    sleep "$DELAY"
  done
  v_actual=$(sha256_of "$v_file")
  # The attested subject digest must be the file's own digest, whatever gh looked up.
  if ! python3 -I - "$v_out" "$v_actual" "$(basename "$v_file")" <<'PY'
import json, sys
path, actual, name = sys.argv[1:4]
results = json.load(open(path))
if not isinstance(results, list) or not results:
    sys.exit("gh reported success but returned no verified attestation")
for r in results:
    statement = r.get("verificationResult", {}).get("statement", {})
    digests = [s.get("digest", {}).get("sha256") for s in statement.get("subject", [])]
    if actual not in digests:
        sys.exit(f"the attested digest(s) {digests} do not include the file's sha256 {actual}")
cert = results[0].get("verificationResult", {}).get("signature", {}).get("certificate", {})
print(f"verified {name}: sha256 {actual}")
for key in ("issuer", "githubWorkflowTrigger", "githubWorkflowRepository", "githubWorkflowRef",
            "githubWorkflowSHA", "buildSignerURI", "sourceRepositoryRef", "runnerEnvironment"):
    if cert.get(key):
        print(f"  {key}: {cert[key]}")
PY
  then
    rm -f "$v_out" "$v_err"
    return 1
  fi
  rm -f "$v_out" "$v_err"
}

case $mode in
  verify)
    [ $# -gt 0 ] || die "verify needs at least one file"
    for f in "$@"; do
      verify_one "$f" || die "NOT VERIFIED: $f"
    done
    echo "verify-provenance: $# file(s) verified against repo $repo, signer workflow $workflow, ref $ref"
    ;;

  negative)
    if [ ! -f "$artifact" ] || [ ! -f "$other" ]; then
      die "negative needs --artifact FILE and --other FILE"
    fi
    # Absolute paths: the bundle is downloaded from inside a temporary directory.
    artifact=$(cd "$(dirname "$artifact")" && pwd)/$(basename "$artifact")
    other=$(cd "$(dirname "$other")" && pwd)/$(basename "$other")
    work=$(mktemp -d "${TMPDIR:-/tmp}/provenance-negative.XXXXXX")
    trap 'rm -rf "$work"' EXIT HUP INT TERM
    base=$(basename "$artifact")
    digest=$(sha256_of "$artifact")

    # The right bundle, fetched from GitHub's own store. Using it for the identity cases means a
    # rejection is about the identity, not about a failed lookup.
    ( cd "$work" && "$GH" attestation download "$artifact" --repo "$repo" >/dev/null ) 2>"$work/dl.err" ||
      { cat "$work/dl.err" >&2; die "could not download the attestation bundle for $base"; }
    bundle=$(find "$work" -maxdepth 1 -name '*.jsonl' | head -n 1)
    [ -n "$bundle" ] || die "gh attestation download produced no bundle"

    echo "== positive controls (these must pass, or the rejections below prove nothing)"
    verify_one "$artifact" || die "control failed: the artifact does not verify by lookup"
    verify_one "$artifact" "$bundle" || die "control failed: the artifact does not verify against its downloaded bundle"

    # The bounded wait for a just-created attestation applies to the controls above only: a case that
    # must be rejected is tried once.
    ATTEMPTS=1
    failed=0
    # expect_rejected NAME -- the last verify_one call's status is passed in $1; its stderr was shown.
    rejected() {
      name=$1
      shift
      if "$@" >"$work/case.out" 2>"$work/case.err"; then
        echo "ACCEPTED (this is a failure): $name" >&2
        failed=1
      else
        echo "rejected: $name"
        echo "    because: $(grep -v '^$' "$work/case.err" "$work/case.out" 2>/dev/null | head -n 2 | tr '\n' ' ' | cut -c1-260)"
      fi
    }

    echo "== negative cases (each must be rejected by gh)"
    # 1. The artifact's bytes changed.
    cp "$artifact" "$work/modified"
    printf 'x' >> "$work/modified"
    rejected "modified artifact (one byte appended), by lookup" verify_one "$work/modified"
    rejected "modified artifact, against the genuine bundle" verify_one "$work/modified" "$bundle"
    # 2. The genuine bundle offered for a different artifact: the attested digest is not this file's.
    rejected "a different artifact offered with this artifact's bundle (digest mismatch)" verify_one "$other" "$bundle"
    # 3. The right bundle, but a different expected repository.
    rejected "unexpected repository" verify_one "$artifact" "$bundle" "rkendel1/not-this-repository" "$workflow" "$ref"
    # 4. A different expected signer workflow in the right repository.
    rejected "unexpected signer workflow" verify_one "$artifact" "$bundle" "$repo" "$repo/.github/workflows/ci.yml" "$ref"
    # 5. A different expected git ref: what a release-tag identity check does to a branch attestation.
    rejected "unexpected git ref (a release-tag identity for this attestation)" verify_one "$artifact" "$bundle" "$repo" "$workflow" "refs/tags/v0.0.0-not-this"
    # 6. A file nobody attested.
    head -c 4096 /dev/urandom > "$work/never-attested" 2>/dev/null || printf 'never attested %s\n' "$$" > "$work/never-attested"
    rejected "a file that was never attested, by lookup" verify_one "$work/never-attested"
    # 7. A bundle whose signature was altered.
    python3 -I - "$bundle" "$work/corrupt.jsonl" <<'PY'
import base64, json, sys
src, dst = sys.argv[1:3]
bundle = json.loads(open(src).read().splitlines()[0])
sig = bundle["dsseEnvelope"]["signatures"][0]
raw = bytearray(base64.b64decode(sig["sig"]))
raw[0] ^= 0xFF
sig["sig"] = base64.b64encode(bytes(raw)).decode()
open(dst, "w").write(json.dumps(bundle) + "\n")
PY
    rejected "a bundle with a corrupted signature" verify_one "$artifact" "$work/corrupt.jsonl"
    # 8. A bundle that is not a bundle.
    printf 'not a bundle\n' > "$work/invalid.jsonl"
    rejected "an invalid bundle file" verify_one "$artifact" "$work/invalid.jsonl"

    [ "$failed" = 0 ] || die "a case that must be rejected was accepted"
    echo "verify-provenance: all negative cases were rejected by gh; the artifact ($digest) verified"
    ;;

  *) die "unknown mode '$mode' (verify or negative)" ;;
esac
