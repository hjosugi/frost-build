#!/usr/bin/env python3
"""End-to-end check that `frost import-dotnet` reproduces MSBuild exactly.

Generates the C# workspace from `frost_bench_csharp`, imports it with
`frost import-dotnet`, builds it with Frost, and requires:

* every produced assembly to be byte-identical to `dotnet build`'s;
* an implementation-only edit to a library to recompile only that library and
  its reference-assembly copy (early cutoff through `/refout`);
* a `const` edit to recompile its consumers;
* a change to a `.csproj` after import to fail the build with the re-import
  message instead of linking stale argv.

Run from the repository root: `python3 scripts/check_import_dotnet.py`.
Cross-platform: it is pure Python and shells out to `frost` and `dotnet`.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import frost_bench_csharp  # noqa: E402


def frost_binary() -> pathlib.Path:
    configured = os.environ.get("FROST_BIN")
    candidates = [
        pathlib.Path(configured) if configured else None,
        ROOT / "target/release/frost",
        ROOT / "target/debug/frost",
    ]
    for candidate in candidates:
        if candidate and candidate.is_file():
            return candidate
    raise SystemExit("build frost first, or set FROST_BIN")


def run(args: list[str], cwd: pathlib.Path, *, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(
        args,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if check and result.returncode != 0:
        raise SystemExit(f"{' '.join(args)} failed:\n{result.stdout}")
    return result


def sha256(path: pathlib.Path) -> str:
    import hashlib

    return hashlib.sha256(path.read_bytes()).hexdigest()


def frost_build(frost: pathlib.Path, root: pathlib.Path, *extra: str) -> subprocess.CompletedProcess:
    return run([str(frost), "-C", str(root), "build", *extra], cwd=root, check=False)


def ran_targets(explain: str) -> set[str]:
    targets = set()
    for line in explain.splitlines():
        line = line.strip()
        if line.startswith("ran command:"):
            targets.add(line.split("command:", 1)[1].split(" ", 1)[0])
    return targets


def reference_build(root: pathlib.Path) -> None:
    run(["dotnet", "build", "src/App/App.csproj", "-c", "Release", "-nologo", "-v:q"], cwd=root)


def reference_assemblies(root: pathlib.Path) -> dict[str, pathlib.Path]:
    found = {}
    for project in ("Core", "Model", "Service", "App"):
        matches = sorted((root / ".dotnet/bin" / project).rglob(f"{project}.dll"))
        if matches:
            found[project] = matches[0]
    return found


def frost_assemblies(root: pathlib.Path) -> dict[str, pathlib.Path]:
    found = {}
    for project in ("Core", "Model", "Service", "App"):
        path = root / f".frost/out/debug/bin/{project}.dll"
        if path.is_file():
            found[project] = path
    return found


def main() -> int:
    if shutil.which("dotnet") is None:
        print("skipping: dotnet is not on PATH")
        return 0
    frost = frost_binary()
    # The generated manifest runs `frost import-check` as its staleness guard,
    # so `frost` must be on PATH for the build the script drives.
    os.environ["PATH"] = str(frost.parent) + os.pathsep + os.environ.get("PATH", "")
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="frost-import-dotnet-"))
    try:
        work = scratch / "work"
        reference = scratch / "reference"
        frost_bench_csharp.generate_csharp_sources(work, 4)
        run(
            [str(frost), "-C", str(work), "import-dotnet", "src/App/App.csproj"],
            cwd=work,
        )
        result = frost_build(frost, work)
        if result.returncode != 0:
            raise SystemExit(f"frost build failed:\n{result.stdout}")

        # A clean MSBuild build of the same sources must match byte for byte.
        shutil.copytree(work, reference)
        for junk in (".frost", ".dotnet-gen", ".dotnet-sdk", "frost.toml"):
            target = reference / junk
            if target.is_dir():
                shutil.rmtree(target)
            elif target.exists():
                target.unlink()
        reference_build(reference)
        frost_outputs = frost_assemblies(work)
        reference_outputs = reference_assemblies(reference)
        if set(frost_outputs) != set(reference_outputs):
            raise SystemExit(
                f"project set differs: {sorted(frost_outputs)} vs {sorted(reference_outputs)}"
            )
        for project, path in frost_outputs.items():
            if sha256(path) != sha256(reference_outputs[project]):
                raise SystemExit(f"{project}.dll is not byte-identical to dotnet build")

        # Implementation-only edit: only the library and its api copy rebuild.
        value_file = work / "src/Core/CoreValue000.cs"
        value_file.write_text(
            value_file.read_text(encoding="utf-8").replace(
                "Get() => 0UL", "Get() => 999999UL"
            ),
            encoding="utf-8",
        )
        explain = frost_build(frost, work, "--explain").stdout
        if ran_targets(explain) != {"core", "core_api"}:
            raise SystemExit(
                f"implementation-only edit ran {sorted(ran_targets(explain))}, "
                "expected only core and core_api"
            )

        # A const edit must reach its consumers.
        constants = work / "src/Core/CoreConstants.cs"
        constants.write_text(
            constants.read_text(encoding="utf-8").replace("Offset = 0UL", "Offset = 7UL"),
            encoding="utf-8",
        )
        explain = frost_build(frost, work, "--explain").stdout
        ran = ran_targets(explain)
        for expected in ("core", "core_api", "model", "service", "app"):
            if expected not in ran:
                raise SystemExit(f"a const change did not recompile {expected}: {sorted(ran)}")

        # A project change after import must fail the build, not link stale argv.
        with (work / "src/Core/Core.csproj").open("a", encoding="utf-8") as handle:
            handle.write("\n<!-- changed after import -->\n")
        refused = frost_build(frost, work)
        if refused.returncode == 0:
            raise SystemExit("a stale import was accepted")
        if "re-run it" not in refused.stdout:
            raise SystemExit(f"a stale import did not name the fix:\n{refused.stdout}")

        print("import-dotnet: byte-identical, early cutoff, const propagation and staleness all pass")
        return 0
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
