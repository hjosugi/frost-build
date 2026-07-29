#!/usr/bin/env bash
# Report remote branches whose every commit is already contained in the default
# branch, and optionally delete them. Deleting such a ref loses no history.
#
# This is the local mirror of .github/workflows/branch-cleanup.yml. The workflow
# is the better default — it also skips branches that still have an open pull
# request, which git alone cannot see — but this works offline from a clone:
#
#     scripts/cleanup-branches.sh            # report only
#     scripts/cleanup-branches.sh --delete   # actually delete
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

usage() {
  cat <<'EOF'
Usage: scripts/cleanup-branches.sh [--delete] [--remote NAME] [--base BRANCH]

  --delete       Delete the branches instead of only listing them.
  --remote NAME  Remote to inspect (default: origin).
  --base BRANCH  Branch to measure containment against (default: the remote's HEAD).
EOF
}

remote="origin"
base=""
delete=0
while [ $# -gt 0 ]; do
  case "$1" in
    --delete) delete=1 ;;
    --remote)
      remote="${2:-}"
      shift
      ;;
    --base)
      base="${2:-}"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "cleanup-branches.sh: unknown argument '$1'" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

git fetch --prune "$remote" >/dev/null 2>&1

if [ -z "$base" ]; then
  base="$(git symbolic-ref --quiet --short "refs/remotes/${remote}/HEAD" 2>/dev/null | sed "s#^${remote}/##" || true)"
  base="${base:-main}"
fi

if ! git rev-parse --verify --quiet "refs/remotes/${remote}/${base}" >/dev/null; then
  echo "cleanup-branches.sh: ${remote}/${base} does not exist" >&2
  exit 1
fi

mapfile -t merged < <(
  git branch -r --merged "${remote}/${base}" --format='%(refname:short)' |
    sed -n "s#^${remote}/##p" |
    grep -vx "$base" |
    grep -vx "HEAD" || true
)

if [ "${#merged[@]}" -eq 0 ]; then
  echo "Nothing to clean up: every ${remote} branch has commits not in ${base}."
  exit 0
fi

echo "Branches fully contained in ${remote}/${base}:"
for branch in "${merged[@]}"; do
  printf '  %-48s %s\n' "$branch" "$(git rev-parse --short "${remote}/${branch}")"
done

if [ "$delete" != "1" ]; then
  cat <<EOF

Nothing deleted. Re-run with --delete to remove them, or use the workflow,
which also protects branches with an open pull request:

    gh workflow run branch-cleanup.yml -f dry_run=false
EOF
  exit 0
fi

echo
git push "$remote" --delete "${merged[@]}"
echo "Deleted ${#merged[@]} branch(es). The SHAs above restore any of them."
