#!/usr/bin/env python3
"""Report the workspace dependencies whose resolved version changed since the
last release.

The changelog records versions, not pull-request titles: a grouped dependabot
pull request is titled with the version it was opened for, which can differ
from the version that resolves by the time it merges. Read Cargo.lock instead.

Run from the repository root, before `scripts/release.sh`:

    python3 scripts/release_changes.py

`--check` exits non-zero unless the `[Unreleased]` section names every changed
dependency and its new version; `release.sh` uses it to refuse a release whose
changelog does not describe what the lockfile ships.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def git(*args: str) -> str:
    return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True)


def latest_tag() -> str:
    return git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*").strip()


def packages(lock: str) -> dict[str, str]:
    return {package["name"]: package["version"] for package in tomllib.loads(lock)["package"]}


def dependencies(manifest: dict, workspace_specs: dict) -> set[str]:
    specs: dict[str, object] = dict(workspace_specs)
    for section in ("dependencies", "build-dependencies"):
        specs.update(manifest.get(section, {}))
    for target in manifest.get("target", {}).values():
        for section in ("dependencies", "build-dependencies"):
            specs.update(target.get(section, {}))

    names = set()
    for key, spec in specs.items():
        if isinstance(spec, dict) and spec.get("workspace") is True:
            spec = workspace_specs.get(key, {})
        if isinstance(spec, dict):
            # A path dependency is another workspace member; its version moves
            # with this release rather than with a bump.
            if "path" in spec:
                continue
            # `alias = { package = "crate", ... }` depends on `crate`.
            names.add(spec.get("package", key))
        else:
            names.add(key)
    return names


def direct_dependencies() -> set[str]:
    root = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    workspace_specs = root.get("workspace", {}).get("dependencies", {})
    names = dependencies(root, workspace_specs)
    for path in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        names |= dependencies(tomllib.loads(path.read_text(encoding="utf-8")), workspace_specs)
    return names


def unreleased(changelog: Path) -> str:
    text = changelog.read_text(encoding="utf-8")
    marker = "## [Unreleased]"
    start = text.find(marker)
    if start < 0:
        return ""
    rest = text[start + len(marker):]
    end = rest.find("\n## [")
    return rest if end < 0 else rest[:end]


def version_pattern(version: str) -> re.Pattern[str]:
    return re.compile(rf"(?<![\d.]){re.escape(version)}(?![\d.])")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--since",
        help="tag to compare against (default: the most recent vX.Y.Z tag)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail unless CHANGELOG.md [Unreleased] names each bump",
    )
    args = parser.parse_args()

    since = args.since or latest_tag()
    before = packages(git("show", f"{since}:Cargo.lock"))
    after = packages((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    direct = direct_dependencies()
    bumps = [
        (name, before[name], after[name])
        for name in sorted(direct)
        if name in before and name in after and before[name] != after[name]
    ]
    if not bumps:
        print(f"No direct dependency version changes since {since}.")
        return 0

    entry = "- Update dependencies: " + ", ".join(
        f"{name} {version}" for name, _, version in bumps
    ) + "."
    section = unreleased(ROOT / "CHANGELOG.md")
    missing = [
        name
        for name, _, version in bumps
        if name not in section or not version_pattern(version).search(section)
    ]
    if args.check and missing:
        print(
            f"CHANGELOG.md [Unreleased] does not describe: {', '.join(missing)}",
            file=sys.stderr,
        )
        print("Suggested entry:", file=sys.stderr)
        print(entry, file=sys.stderr)
        return 1

    print(entry)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
