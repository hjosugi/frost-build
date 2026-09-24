#!/usr/bin/env bash
# Verify a downloaded FrostBuild release: signatures, checksums, provenance
# and SBOMs. This is the procedure README.md and docs/30_distribution.md
# publish, as one command; each step below is one of the commands printed
# there, run for every asset instead of one.
#
#   scripts/verify_release.sh DIR VERSION
#
# DIR holds the release's assets as published (at least SHA256SUMS, its
# .sigstore.json bundle, the archives, their bundles and SBOMs, and
# frostbuild-vVERSION.intoto.jsonl). Needs cosign and an authenticated gh.
#
# The release workflow runs this on its own output before it creates the tag,
# and the distribution smoke job runs it daily against the real latest
# release, so the published commands are exercised by CI, not only written
# down. The two options exist for the dry-run workflow, whose signatures come
# from a different workflow file and may come from a branch; a user verifying
# a release never needs them.
set -euo pipefail

repository="hjosugi/frost-build"
issuer="https://token.actions.githubusercontent.com"
signer_workflow=".github/workflows/release.yml"
# A release is signed by release.yml running on main (workflow_dispatch) or
# on its own tag (tag push). Anything else is not a release.
source_ref='refs/(heads/main|tags/v[0-9]+\.[0-9]+\.[0-9]+)'

usage() {
  cat <<'EOF'
Usage: scripts/verify_release.sh [--signer-workflow PATH] [--source-ref-regexp RE] DIR VERSION

Verifies every archive listed in DIR/SHA256SUMS:
  1. cosign: SHA256SUMS was signed by the release workflow of hjosugi/frost-build
  2. SHA-256: every archive matches SHA256SUMS
  3. cosign: every archive carries the same workflow's signature
  4. SBOM: every archive has a CycloneDX SBOM describing this version
  5. gh attestation: archives, SBOMs and SHA256SUMS have SLSA provenance from it
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
  --signer-workflow)
    [ "$#" -ge 2 ] || { usage >&2; exit 2; }
    signer_workflow="$2"
    shift 2
    ;;
  --source-ref-regexp)
    [ "$#" -ge 2 ] || { usage >&2; exit 2; }
    source_ref="$2"
    shift 2
    ;;
  -h | --help) usage; exit 0 ;;
  -*) echo "verify_release.sh: unknown option '$1'" >&2; usage >&2; exit 2 ;;
  *) break ;;
  esac
done
if [ "$#" -ne 2 ]; then
  usage >&2
  exit 2
fi
dir="$1"
version="${2#v}"
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "verify_release.sh: '$version' is not an X.Y.Z version" >&2
  exit 2
fi

fail() {
  echo "verify_release.sh: FAILED: $1" >&2
  exit 1
}

for tool in cosign gh python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "$tool is not installed"
done
if command -v sha256sum >/dev/null 2>&1; then
  sha256_check() { sha256sum --check --strict "$1"; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_check() { shasum --algorithm 256 --check --strict "$1"; }
else
  fail "neither sha256sum nor shasum is available"
fi

escaped_workflow="$(printf '%s' "$signer_workflow" | sed 's/[].[^$*+?(){}|\\]/\\&/g')"
identity="^https://github\\.com/${repository//./\\.}/${escaped_workflow}@${source_ref}\$"
provenance="$dir/frostbuild-v${version}.intoto.jsonl"

for required in SHA256SUMS SHA256SUMS.sigstore.json "${provenance##*/}"; do
  [ -f "$dir/$required" ] || fail "$dir has no $required"
done

archives=()
while read -r _digest name; do
  name="${name#\*}"
  case "$name" in
  frostbuild-v"$version"-*.tar.gz | frostbuild-v"$version"-*.zip) archives+=("$name") ;;
  *) fail "SHA256SUMS lists '$name', which is not a v$version archive" ;;
  esac
done <"$dir/SHA256SUMS"
[ "${#archives[@]}" -gt 0 ] || fail "SHA256SUMS lists no archive"

echo "==> 1. cosign: SHA256SUMS signed by $signer_workflow"
cosign verify-blob \
  --bundle "$dir/SHA256SUMS.sigstore.json" \
  --certificate-identity-regexp "$identity" \
  --certificate-oidc-issuer "$issuer" \
  "$dir/SHA256SUMS" || fail "the SHA256SUMS signature does not verify"

echo "==> 2. SHA-256: archives match SHA256SUMS"
(cd "$dir" && sha256_check SHA256SUMS) || fail "an archive does not match SHA256SUMS"

subjects=("$dir/SHA256SUMS")
for archive in "${archives[@]}"; do
  echo "==> 3. cosign: $archive"
  [ -f "$dir/$archive.sigstore.json" ] || fail "$archive has no .sigstore.json signature bundle"
  cosign verify-blob \
    --bundle "$dir/$archive.sigstore.json" \
    --certificate-identity-regexp "$identity" \
    --certificate-oidc-issuer "$issuer" \
    "$dir/$archive" || fail "the signature of $archive does not verify"

  stem="${archive%.tar.gz}"
  stem="${stem%.zip}"
  sbom="$dir/$stem.cdx.json"
  [ -f "$sbom" ] || fail "$archive has no SBOM $stem.cdx.json"
  echo "==> 4. SBOM: ${sbom##*/}"
  python3 - "$sbom" "$version" <<'PY' || fail "${sbom##*/} is not this release's CycloneDX SBOM"
import json
import sys

path, version = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    bom = json.load(handle)
component = bom.get("metadata", {}).get("component", {})
if bom.get("bomFormat") != "CycloneDX" or not bom.get("components"):
    sys.exit(f"{path}: not a CycloneDX document with components")
if (component.get("name"), component.get("version")) != ("frostbuild-cli", version):
    sys.exit(f"{path}: describes {component.get('name')} {component.get('version')}")
PY
  subjects+=("$dir/$archive" "$sbom")
done

for subject in "${subjects[@]}"; do
  echo "==> 5. provenance: ${subject##*/}"
  gh attestation verify "$subject" \
    --bundle "$provenance" \
    --repo "$repository" \
    --signer-workflow "$repository/$signer_workflow" >/dev/null ||
    fail "${subject##*/} has no valid provenance from $repository/$signer_workflow"
done

echo "verify_release.sh: v$version verified: ${#archives[@]} archives, their SBOMs and SHA256SUMS"
