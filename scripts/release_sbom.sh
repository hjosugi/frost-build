#!/usr/bin/env bash
# Write a CycloneDX SBOM next to every release archive in a directory.
#
#   scripts/release_sbom.sh DIST VERSION
#
# For each DIST/frostbuild-vVERSION-<triple>.{tar.gz,zip} this writes
# DIST/frostbuild-vVERSION-<triple>.cdx.json: the resolved dependency graph of
# frostbuild-cli for exactly that target triple, from Cargo.lock, by
# cargo-cyclonedx. frostbuild-cli depends on frostbuild-daemon, so the one
# document covers the crates in both `frost` and `frostd`; platform-only crates
# (windows-sys, for instance) appear only in the SBOM of the platform that links
# them. SOURCE_DATE_EPOCH (default: the HEAD commit time) fixes the document's
# timestamp and suppresses its random serial number, so regenerating it from
# the same commit gives the same bytes.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "$#" -ne 2 ]; then
  echo "usage: scripts/release_sbom.sh DIST VERSION" >&2
  exit 2
fi
dist="$1"
version="${2#v}"
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "release_sbom.sh: '$version' is not an X.Y.Z version" >&2
  exit 2
fi
if ! cargo cyclonedx --version >/dev/null 2>&1; then
  echo "release_sbom.sh: cargo-cyclonedx is not installed (cargo install --locked cargo-cyclonedx)" >&2
  exit 2
fi

export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct)}"

shopt -s nullglob
archives=("$dist"/frostbuild-v"$version"-*.tar.gz "$dist"/frostbuild-v"$version"-*.zip)
if [ "${#archives[@]}" -eq 0 ]; then
  echo "release_sbom.sh: $dist holds no frostbuild-v$version-* archive" >&2
  exit 1
fi

# cargo-cyclonedx writes one file per workspace member, next to each member's
# manifest. The name is unique to this run and every copy is removed, so a
# failure part-way leaves nothing behind in the source tree.
scratch="release-sbom-$$"
cleanup() {
  rm -f "$root"/crates/*/"$scratch".json
}
trap cleanup EXIT

for archive in "${archives[@]}"; do
  name="${archive##*/}"
  stem="${name%.tar.gz}"
  stem="${stem%.zip}"
  triple="${stem#frostbuild-v"$version"-}"
  cargo cyclonedx --quiet \
    --manifest-path "$root/crates/frostbuild-cli/Cargo.toml" \
    --format json --spec-version 1.5 \
    --target "$triple" \
    --override-filename "$scratch"
  output="$dist/$stem.cdx.json"
  mv "$root/crates/frostbuild-cli/$scratch.json" "$output"
  cleanup

  # A document that parses but describes something else is worse than none.
  python3 - "$output" "$version" <<'PY'
import json
import sys

path, version = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    bom = json.load(handle)
component = bom.get("metadata", {}).get("component", {})
names = {item.get("name") for item in bom.get("components", [])}
problems = []
if bom.get("bomFormat") != "CycloneDX":
    problems.append("bomFormat is not CycloneDX")
if component.get("name") != "frostbuild-cli":
    problems.append(f"describes {component.get('name')!r}, not frostbuild-cli")
if component.get("version") != version:
    problems.append(f"describes version {component.get('version')!r}, not {version}")
for required in ("frostbuild-core", "frostbuild-daemon", "frostbuild-exec", "blake3"):
    if required not in names:
        problems.append(f"lists no {required} component")
if problems:
    sys.exit(f"release_sbom.sh: {path}: " + "; ".join(problems))
print(f"{path}: CycloneDX {bom['specVersion']}, {len(names)} components")
PY
done
