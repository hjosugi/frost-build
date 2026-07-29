#!/usr/bin/env bash
# The pre-PR gate from CONTRIBUTING.md, in one command. Runs everything even
# after a failure, then reports every stage that failed, so one run tells you
# the whole story instead of one problem at a time.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

failed=()

stage() {
  local name="$1"
  shift
  echo
  echo "==> ${name}"
  if "$@"; then
    return 0
  fi
  failed+=("$name")
  return 0
}

stage "cargo test" cargo test --workspace --all-targets --locked
stage "cargo clippy" cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
stage "cargo fmt" cargo fmt --all -- --check
stage "python tests" python3 -m unittest discover -s tests

echo
if [ "${#failed[@]}" -eq 0 ]; then
  echo "All checks passed."
  exit 0
fi

echo "FAILED: ${failed[*]}"
exit 1
