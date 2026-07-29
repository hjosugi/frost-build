#!/usr/bin/env bash
# Prepare a release commit: bump the workspace version, stamp the CHANGELOG
# and refresh the lockfile.
#
# This deliberately does not create or push a tag. The tag, the three platform
# archives and the GitHub release all come from .github/workflows/release.yml,
# which can be started from the Actions UI or with:
#
#     gh workflow run release.yml -f version=X.Y.Z
#
# so cutting a release never depends on what a particular laptop can push.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

usage() {
  cat <<'EOF'
Usage: scripts/release.sh <version> [--push]

  <version>   Release version, X.Y.Z (a leading "v" is accepted and stripped).
  --push      Push the current branch after committing.

Then open a PR, merge it, and run the Release workflow for the same version.
EOF
}

version=""
push=0
while [ $# -gt 0 ]; do
  case "$1" in
    --push) push=1 ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      echo "release.sh: unknown option '$1'" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [ -n "$version" ]; then
        echo "release.sh: version given twice" >&2
        usage >&2
        exit 2
      fi
      version="${1#v}"
      ;;
  esac
  shift
done

if [ -z "$version" ]; then
  usage >&2
  exit 2
fi

if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "release.sh: '$version' is not a X.Y.Z version" >&2
  exit 2
fi

if [ -n "$(git status --porcelain)" ]; then
  echo "release.sh: the working tree has uncommitted changes; commit or stash them first" >&2
  exit 1
fi

current="$(awk '
  /^\[workspace\.package\]/ { in_section = 1; next }
  /^\[/                     { in_section = 0 }
  in_section && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
' Cargo.toml)"

if [ "$current" = "$version" ]; then
  echo "release.sh: Cargo.toml is already at $version" >&2
  exit 1
fi

if grep -q "^## \[${version}\] - " CHANGELOG.md; then
  echo "release.sh: CHANGELOG.md already has a $version section" >&2
  exit 1
fi

# A release with an empty Unreleased section ships nothing anyone can read.
unreleased="$(awk '
  /^## \[Unreleased\]/ { in_section = 1; next }
  /^## \[/             { in_section = 0 }
  in_section           { print }
' CHANGELOG.md | tr -d '[:space:]')"
if [ -z "$unreleased" ]; then
  echo "release.sh: CHANGELOG.md has nothing under '## [Unreleased]'" >&2
  exit 1
fi

echo "Releasing ${current} -> ${version}"

# The workspace version and the path-dependency pins move together; leaving one
# behind produces a workspace that resolves to the previous release.
perl -pi -e "s/^version = \"\Q${current}\E\"$/version = \"${version}\"/" Cargo.toml
perl -pi -e "s/\{ version = \"\Q${current}\E\", path = \"crates\//{ version = \"${version}\", path = \"crates\//" Cargo.toml

remaining="$(grep -c "\"${current}\"" Cargo.toml || true)"
if [ "$remaining" != "0" ]; then
  echo "release.sh: Cargo.toml still mentions ${current} after the bump:" >&2
  grep -n "\"${current}\"" Cargo.toml >&2
  exit 1
fi

# Cargo.lock records the workspace member versions too, and `--locked` builds
# in CI fail if it is stale.
cargo update --workspace --offline >/dev/null

awk -v header="## [${version}] - $(date -u +%Y-%m-%d)" '
  !stamped && /^## \[Unreleased\]$/ { print; print ""; print header; stamped = 1; next }
  { print }
' CHANGELOG.md >CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -q -m "Release ${version}"
echo "Committed: $(git log --oneline -1)"

if [ "$push" = "1" ]; then
  branch="$(git rev-parse --abbrev-ref HEAD)"
  git push -u origin "$branch"
fi

cat <<EOF

Next:
  1. Open a PR for this commit and merge it into the default branch.
  2. Publish the release (no local tag needed):
       gh workflow run release.yml -f version=${version}
EOF
