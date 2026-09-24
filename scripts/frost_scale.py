#!/usr/bin/env python3
"""Large-workspace scale, soak and recovery evidence for the v1 quality gate.

Three subcommands, one generator:

    generate   write a synthetic workspace of a named shape, deterministically
    scale      cold plan / warm no-op / one-file change / header change, with
               peak RSS and `.frost/` size, against a clean-build oracle
    soak       keep `frost daemon serve` and `frost watch` resident while a
               seeded stream of edits lands, sampling fds, RSS and journal size

The existing generators (`frost_bench.py`, `frost-bench-rs daemon-graph`) make
one linear chain so that Frost, Ninja, Make and Bazel can be given the same
graph. That is the right shape for a head-to-head and the wrong one for finding
where Frost itself stops scaling, which needs width, many manifests, generated
headers feeding real compiles, and owned output trees. Every shape here is a
preset of one `Shape`; nothing is random except through `Shape.seed`.

Every genrule output is `cksum` of what it reads, so a change always propagates
(no accidental early cutoff), outputs stay a few bytes whatever the depth, and
the harness can predict both the content of a leaf's output and exactly which
actions a change must rerun. Correctness is judged two ways: that prediction,
and a byte-for-byte comparison with a from-scratch build of the same sources in
a fresh directory.

The harness needs a POSIX shell, `cksum` and, for shapes with C sources, a C
compiler. `soak` samples `/proc` and is Linux-only.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import pathlib
import platform
import random
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone
from typing import Any, Iterable

SCALE_SCHEMA = "frost-scale-v1"
SOAK_SCHEMA = "frost-soak-v1"
GENERATOR_VERSION = 1
CONFIG = "debug"

# The journal is rewritten once it passes this size (crates/frostbuild-exec,
# `Engine::run`); the soak asserts it never grows unboundedly past it.
JOURNAL_COMPACTION_BYTES = 32 * 1024 * 1024


@dataclasses.dataclass(frozen=True)
class Shape:
    """One synthetic workspace. Presets below; every field can be overridden."""

    name: str
    targets: int
    packages: int = 1
    # 0 builds a linear chain inside each package; otherwise leaves are joined
    # by a fan-in tree whose nodes read at most `fanout` children.
    fanout: int = 0
    files_per_target: int = 1
    # Per package: generated headers from one genrule, compiled by `cc_sources`
    # translation units of one cc_library.
    generated_headers: int = 0
    cc_sources: int = 0
    # Workspace-wide command targets owning an output directory each.
    output_dir_targets: int = 0
    output_dir_files: int = 0
    # Each package root also reads up to this many earlier package roots.
    cross_package_deps: int = 0
    seed: int = 149

    def validate(self) -> None:
        if self.targets < 1:
            raise ValueError("targets must be at least 1")
        if not 1 <= self.packages <= self.targets:
            raise ValueError("packages must be between 1 and targets")
        if self.fanout == 1 or self.fanout < 0:
            raise ValueError("fanout must be 0 (linear) or at least 2")
        if self.files_per_target < 1:
            raise ValueError("files_per_target must be at least 1")
        if self.cc_sources and not self.generated_headers:
            raise ValueError("cc_sources needs generated_headers to include")
        if self.output_dir_targets and self.output_dir_files < 1:
            raise ValueError("output_dir_files must be at least 1")


PRESETS: dict[str, Shape] = {
    "linear": Shape("linear", targets=1000),
    "wide": Shape("wide", targets=1000, fanout=32),
    "packages": Shape("packages", targets=1000, packages=50, fanout=8, cross_package_deps=2),
    "headers": Shape(
        "headers", targets=200, packages=10, fanout=8, generated_headers=20, cc_sources=2
    ),
    "output-dirs": Shape(
        "output-dirs", targets=100, fanout=8, output_dir_targets=10, output_dir_files=200
    ),
    # The soak default: every feature at a size a laptop rebuilds in seconds.
    "soak": Shape(
        "soak",
        targets=2000,
        packages=40,
        fanout=8,
        generated_headers=4,
        cc_sources=1,
        output_dir_targets=4,
        output_dir_files=100,
        cross_package_deps=2,
    ),
    # A monorepo-sized target: 50k targets and 100k source files.
    "monorepo": Shape(
        "monorepo",
        targets=50_000,
        packages=500,
        fanout=16,
        files_per_target=2,
        generated_headers=8,
        cc_sources=2,
        output_dir_targets=20,
        output_dir_files=500,
        cross_package_deps=2,
    ),
}


@dataclasses.dataclass
class TargetModel:
    label: str
    kind: str  # genrule | cc_library | command
    actions: int
    deps: list[str]
    # Workspace-relative files read directly (sources, not dependency outputs).
    sources: list[str]
    outputs: list[str]
    output_dirs: list[str] = dataclasses.field(default_factory=list)
    # For a cksum genrule: the ordered files it concatenates, sources and
    # dependency outputs alike; None when the output is not predictable here.
    reads: list[str] | None = None


@dataclasses.dataclass
class WorkspaceModel:
    shape: Shape
    targets: dict[str, TargetModel]
    leaf_sources: list[str]
    header_inputs: list[str]
    tree_inputs: list[str]
    files: int
    bytes: int

    @property
    def actions(self) -> int:
        return sum(target.actions for target in self.targets.values())

    def rdeps(self) -> dict[str, list[str]]:
        reverse: dict[str, list[str]] = {label: [] for label in self.targets}
        for target in self.targets.values():
            for dep in target.deps:
                reverse[dep].append(target.label)
        return reverse

    def affected_actions(self, changed: Iterable[str]) -> int:
        """Actions a content change to `changed` must rerun (no early cutoff)."""
        changed = set(changed)
        seeds = [t.label for t in self.targets.values() if changed.intersection(t.sources)]
        reverse = self.rdeps()
        seen: set[str] = set()
        stack = list(seeds)
        while stack:
            label = stack.pop()
            if label in seen:
                continue
            seen.add(label)
            stack.extend(reverse[label])
        return sum(self.targets[label].actions for label in seen)

    def declared_outputs(self) -> list[str]:
        paths: list[str] = []
        for target in self.targets.values():
            paths.extend(target.outputs)
        return sorted(paths)

    def declared_output_dirs(self) -> list[str]:
        paths: list[str] = []
        for target in self.targets.values():
            paths.extend(target.output_dirs)
        return sorted(paths)


# ---------------------------------------------------------------------------
# POSIX cksum, so a leaf's expected output can be computed without running it.


def _cksum_table() -> list[int]:
    table = []
    for byte in range(256):
        crc = byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) if crc & 0x80000000 else (crc << 1)
            crc &= 0xFFFFFFFF
        table.append(crc)
    return table


_CKSUM_TABLE = _cksum_table()


def posix_cksum(data: bytes) -> str:
    """The line `cksum` prints for `data` on stdin: `CRC SIZE\\n`."""
    crc = 0
    for byte in data:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ _CKSUM_TABLE[((crc >> 24) ^ byte) & 0xFF]
    length = len(data)
    while length:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ _CKSUM_TABLE[((crc >> 24) ^ (length & 0xFF)) & 0xFF]
        length >>= 8
    return f"{(~crc) & 0xFFFFFFFF} {len(data)}\n"


# ---------------------------------------------------------------------------
# Generation


def toml_string(value: str) -> str:
    # A JSON string is a valid TOML basic string for everything emitted here.
    return json.dumps(value)


def toml_list(values: Iterable[str]) -> str:
    return "[" + ", ".join(toml_string(value) for value in values) + "]"


def package_dir(index: int) -> str:
    # Two levels, so discovery walks nested directories rather than one flat
    # listing of hundreds of manifests.
    return f"pkgs/g{index // 50:02d}/p{index:04d}"


def package_label(index: int, name: str) -> str:
    return f"//{package_dir(index)}:{name}"


class _Writer:
    def __init__(self, root: pathlib.Path) -> None:
        self.root = root
        self.files = 0
        self.bytes = 0

    def write(self, rel: str, content: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        data = content.encode("utf-8")
        path.write_bytes(data)
        self.files += 1
        self.bytes += len(data)


# A child whose output an aggregator reads but whose bytes this module cannot
# predict (a C archive, an owned tree's contents stamp).
UNPREDICTABLE = "?"

CKSUM_GENRULE = "cat {reads} | cksum > ${{out}}"
HEADERS_GENRULE = (
    "v=$(cksum < ${in} | cut -d' ' -f1); "
    "for f in ${outs}; do n=$(basename $f .h); "
    "printf '#define %s_VALUE (%su)\\n' $n $v > $f; done"
)
# Fills an owned directory with `n` files and writes a one-line summary beside
# it, because a target that owns only a directory has no path `${dep:}` can
# name, and an aggregator must read something that changes when the tree does.
TREE_SCRIPT = (
    'd=$1; n=$2; o=$3; shift 3; mkdir -p "$d"; '
    'v=$(cat "$@" | cksum | cut -d" " -f1); i=0; '
    'while [ $i -lt $n ]; do printf "%s %s\\n" $i $v > "$d/f$i.txt"; i=$((i+1)); done; '
    'printf "%s %s\\n" $v $n > "$o"'
)


def _fan_in(
    children: list[tuple[str, str]],
    fanout: int,
    prefix: str,
    make: Any,
) -> tuple[str, str]:
    """Join (label, output) children into one target named `prefix`.

    No aggregator reads more than `fanout` children; `make(name, group)` emits
    one aggregator and returns its (label, output).
    """
    width = fanout if fanout >= 2 else max(2, len(children))
    layer = 0
    while len(children) > width:
        children = [
            make(f"{prefix}_l{layer}_{index:05d}", children[start : start + width])
            for index, start in enumerate(range(0, len(children), width))
        ]
        layer += 1
    return make(prefix, children)


def generate(root: pathlib.Path, shape: Shape) -> WorkspaceModel:
    """Write `shape` into `root` (which must be empty or absent)."""
    shape.validate()
    if root.exists() and any(root.iterdir()):
        raise ValueError(f"{root} is not empty")
    root.mkdir(parents=True, exist_ok=True)
    rng = random.Random(shape.seed)
    writer = _Writer(root)
    targets: dict[str, TargetModel] = {}
    leaf_sources: list[str] = []
    header_inputs: list[str] = []
    tree_inputs: list[str] = []
    package_roots: list[tuple[str, str]] = []

    per_package = [shape.targets // shape.packages] * shape.packages
    for index in range(shape.targets % shape.packages):
        per_package[index] += 1

    next_target = 0
    for pkg in range(shape.packages):
        pdir = package_dir(pkg)
        lines = [f"# Generated by scripts/frost_scale.py — shape {shape.name}, package {pkg}.", ""]

        def local(name: str, pdir: str = pdir) -> str:
            return f"//{pdir}:{name}"

        leaves: list[tuple[str, str]] = []
        previous: tuple[str, str] | None = None
        for _ in range(per_package[pkg]):
            name = f"t{next_target:05d}"
            next_target += 1
            sources = [f"src/{name}_{k}.txt" for k in range(shape.files_per_target)]
            for rel in sources:
                writer.write(f"{pdir}/{rel}", f"{name} value={rng.randrange(1 << 30)}\n")
            ws_sources = [f"{pdir}/{rel}" for rel in sources]
            output = f"out/{name}.sum"
            deps: list[str] = []
            reads = list(ws_sources)
            dep_refs = ""
            if shape.fanout == 0 and previous is not None:
                deps = [f":{previous[0].rsplit(':', 1)[1]}"]
                reads.append(previous[1])
                dep_refs = " ${dep:" + deps[0] + "}"
            lines += [
                f"[target.{name}]",
                'kind = "genrule"',
                f"cmd = {toml_string(CKSUM_GENRULE.format(reads='${in}' + dep_refs))}",
                f"inputs = {toml_list(sources)}",
                f"deps = {toml_list(deps)}",
                f"outputs = {toml_list([output])}",
                "",
            ]
            label = local(name)
            targets[label] = TargetModel(
                label=label,
                kind="genrule",
                actions=1,
                deps=[local(dep[1:]) for dep in deps],
                sources=ws_sources,
                outputs=[f"{pdir}/{output}"],
                reads=reads,
            )
            leaf_sources.extend(ws_sources)
            previous = (label, f"{pdir}/{output}")
            leaves.append(previous)

        extra: list[tuple[str, str]] = []
        if shape.generated_headers:
            headers = [f"gen/p{pkg:04d}_h{k:03d}.h" for k in range(shape.generated_headers)]
            writer.write(f"{pdir}/headers.in", f"package {pkg} seed={rng.randrange(1 << 30)}\n")
            header_inputs.append(f"{pdir}/headers.in")
            lines += [
                "[target.headers]",
                'kind = "genrule"',
                f"cmd = {toml_string(HEADERS_GENRULE)}",
                'inputs = ["headers.in"]',
                'includes = ["gen"]',
                f"outputs = {toml_list(headers)}",
                "",
            ]
            targets[local("headers")] = TargetModel(
                label=local("headers"),
                kind="genrule",
                actions=1,
                deps=[],
                sources=[f"{pdir}/headers.in"],
                outputs=[f"{pdir}/{h}" for h in headers],
            )
            if shape.cc_sources:
                for unit in range(shape.cc_sources):
                    chosen = sorted(
                        rng.sample(range(shape.generated_headers), min(3, shape.generated_headers))
                    )
                    includes = "".join(f'#include "p{pkg:04d}_h{k:03d}.h"\n' for k in chosen)
                    total = " + ".join(f"p{pkg:04d}_h{k:03d}_VALUE" for k in chosen)
                    writer.write(
                        f"{pdir}/csrc/u{unit:02d}.c",
                        f"{includes}\nunsigned p{pkg:04d}_u{unit:02d}(void) {{ return {total}; }}\n",
                    )
                lines += [
                    "[target.lib]",
                    'kind = "cc_library"',
                    'srcs = ["csrc/*.c"]',
                    'deps = [":headers"]',
                    "",
                ]
                # One compile per translation unit plus the archive.
                targets[local("lib")] = TargetModel(
                    label=local("lib"),
                    kind="cc_library",
                    actions=shape.cc_sources + 1,
                    deps=[local("headers")],
                    sources=[f"{pdir}/csrc/u{unit:02d}.c" for unit in range(shape.cc_sources)],
                    outputs=[],
                )
                extra.append((local("lib"), UNPREDICTABLE))

        cross: list[tuple[str, str]] = []
        if shape.cross_package_deps and pkg:
            picks = sorted(rng.sample(range(pkg), min(shape.cross_package_deps, pkg)))
            cross = [package_roots[p] for p in picks]

        def make_aggregator(
            name: str,
            group: list[tuple[str, str]],
            lines: list[str] = lines,
            local: Any = local,
            pdir: str = pdir,
        ) -> tuple[str, str]:
            refs = [child[0] for child in group]
            readable = [child for child in group if child[1]]
            output = f"out/{name}.sum"
            dep_refs = " ".join("${dep:" + ref + "}" for ref, _ in readable)
            predictable = all(child[1] not in ("", UNPREDICTABLE) for child in group)
            lines.extend(
                [
                    f"[target.{name}]",
                    'kind = "genrule"',
                    f"cmd = {toml_string(CKSUM_GENRULE.format(reads=dep_refs))}",
                    f"deps = {toml_list(refs)}",
                    f"outputs = {toml_list([output])}",
                    "",
                ]
            )
            label = local(name)
            targets[label] = TargetModel(
                label=label,
                kind="genrule",
                actions=1,
                deps=list(refs),
                sources=[],
                outputs=[f"{pdir}/{output}"],
                reads=[path for _, path in readable] if predictable else None,
            )
            return label, f"{pdir}/{output}"

        # A chain's last link already reads every earlier one.
        children = [leaves[-1]] if shape.fanout == 0 else leaves
        children = children + extra + cross
        root_label, root_output = _fan_in(children, shape.fanout, "root", make_aggregator)
        package_roots.append((root_label, root_output))
        writer.write(f"{pdir}/frost.toml", "\n".join(lines))

    root_lines = [
        f"# Generated by scripts/frost_scale.py (generator v{GENERATOR_VERSION}).",
        f"# shape = {json.dumps(dataclasses.asdict(shape), sort_keys=True)}",
        "",
        "[workspace]",
        f"name = {toml_string('scale-' + shape.name)}",
        'default_targets = ["//:all"]',
        "",
    ]
    if shape.cc_sources:
        root_lines += ["[toolchain]", 'cflags = ["-O0"]', ""]
    if shape.output_dir_targets:
        root_lines += ["[toolchain.tools]", 'sh = "sh"', ""]

    trees: list[tuple[str, str]] = []
    for index in range(shape.output_dir_targets):
        name = f"tree{index:03d}"
        rel_input = f"trees/{name}.in"
        writer.write(rel_input, f"{name} seed={rng.randrange(1 << 30)}\n")
        tree_inputs.append(rel_input)
        out_dir = f"trees/out/${{config}}/{name}"
        summary = f"trees/out/${{config}}/{name}.sum"
        args = ["-c", TREE_SCRIPT, "fill", "${output_dir}", str(shape.output_dir_files)]
        root_lines += [
            f"[target.{name}]",
            'kind = "command"',
            'tool = "sh"',
            f"args = {toml_list(args + ['${out}', '${in}'])}",
            f"inputs = {toml_list([rel_input])}",
            f"outputs = {toml_list([summary])}",
            f"output_dirs = {toml_list([out_dir])}",
            "",
        ]
        label = f"//:{name}"
        targets[label] = TargetModel(
            label=label,
            kind="command",
            actions=1,
            deps=[],
            sources=[rel_input],
            outputs=[summary.replace("${config}", CONFIG)],
            output_dirs=[out_dir.replace("${config}", CONFIG)],
        )
        trees.append((label, UNPREDICTABLE))

    def spelled(label: str) -> str:
        # Frost names a root-package target by its bare name, and `${dep:}`
        # must spell a dependency exactly as `deps` does.
        return label[3:] if label.startswith("//:") else label

    def make_root(name: str, group: list[tuple[str, str]]) -> tuple[str, str]:
        refs = [child[0] for child in group]
        readable = [child for child in group if child[1]]
        output = f"out/{name}.sum"
        dep_refs = " ".join("${dep:" + spelled(ref) + "}" for ref, _ in readable)
        root_lines.extend(
            [
                f"[target.{name}]",
                'kind = "genrule"',
                f"cmd = {toml_string(CKSUM_GENRULE.format(reads=dep_refs or '/dev/null'))}",
                f"deps = {toml_list(spelled(ref) for ref in refs)}",
                f"outputs = {toml_list([output])}",
                "",
            ]
        )
        label = f"//:{name}"
        targets[label] = TargetModel(
            label=label,
            kind="genrule",
            actions=1,
            deps=list(refs),
            sources=[],
            outputs=[output],
            reads=(
                [path for _, path in readable]
                if all(child[1] not in ("", UNPREDICTABLE) for child in group)
                else None
            ),
        )
        return label, output

    _fan_in(package_roots + trees, shape.fanout or 64, "all", make_root)
    writer.write("frost.toml", "\n".join(root_lines))
    return WorkspaceModel(
        shape=shape,
        targets=targets,
        leaf_sources=leaf_sources,
        header_inputs=header_inputs,
        tree_inputs=tree_inputs,
        files=writer.files,
        bytes=writer.bytes,
    )


def tree_digest(root: pathlib.Path, *, exclude: Iterable[str] = (".frost",)) -> str:
    """SHA-256 over every relative path and its bytes, in sorted order."""
    excluded = set(exclude)
    digest = hashlib.sha256()
    paths = []
    for path in root.rglob("*"):
        rel = path.relative_to(root)
        if rel.parts and rel.parts[0] in excluded:
            continue
        if path.is_file():
            paths.append(rel.as_posix())
    for rel in sorted(paths):
        digest.update(rel.encode("utf-8") + b"\0")
        digest.update((root / rel).read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def shape_from_args(args: argparse.Namespace) -> Shape:
    shape = PRESETS[args.shape]
    overrides = {}
    for field in dataclasses.fields(Shape):
        value = getattr(args, field.name, None)
        if field.name != "name" and value is not None:
            overrides[field.name] = value
    return dataclasses.replace(shape, **overrides)


# ---------------------------------------------------------------------------
# Measurement helpers


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def environment() -> dict[str, Any]:
    load = os.getloadavg() if hasattr(os, "getloadavg") else None
    governor = None
    try:
        governor = pathlib.Path(
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"
        ).read_text().strip()
    except OSError:
        pass
    memory_kib = None
    try:
        for line in pathlib.Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                memory_kib = int(line.split()[1])
    except OSError:
        pass
    return {
        "captured_at": utc_now(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
        "memory_kib": memory_kib,
        "load_avg": list(load) if load else None,
        "cpu_governor": governor,
        "github_runner": os.environ.get("RUNNER_NAME"),
        "github_run": (
            f"{os.environ['GITHUB_SERVER_URL']}/{os.environ['GITHUB_REPOSITORY']}"
            f"/actions/runs/{os.environ['GITHUB_RUN_ID']}"
            if "GITHUB_RUN_ID" in os.environ
            else None
        ),
    }


@dataclasses.dataclass
class Run:
    argv: list[str]
    code: int
    elapsed_ms: float
    max_rss_kib: int
    stdout: str
    stderr: str


def run_measured(
    argv: list[str],
    *,
    cwd: pathlib.Path | None = None,
    env: dict[str, str] | None = None,
    timeout_s: float = 3600,
) -> Run:
    """Run to completion, returning wall time and the child's peak RSS."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        started = time.perf_counter()
        proc = subprocess.Popen(argv, cwd=cwd, env=env, stdout=out, stderr=err)
        timer = threading.Timer(timeout_s, proc.kill)
        timer.start()
        try:
            _, status, usage = os.wait4(proc.pid, 0)
        finally:
            timer.cancel()
        elapsed = (time.perf_counter() - started) * 1000.0
        code = os.waitstatus_to_exitcode(status)
        proc.returncode = code  # already reaped; keep Popen from waiting again
        rss = usage.ru_maxrss // 1024 if sys.platform == "darwin" else usage.ru_maxrss
        out.seek(0)
        err.seek(0)
        return Run(
            argv=argv,
            code=code,
            elapsed_ms=elapsed,
            max_rss_kib=int(rss),
            stdout=out.read().decode("utf-8", "replace"),
            stderr=err.read().decode("utf-8", "replace"),
        )


def summarize(samples: list[float]) -> dict[str, Any]:
    ordered = sorted(samples)
    return {
        "samples": [round(value, 3) for value in samples],
        "median": round(statistics.median(ordered), 3),
        "min": round(ordered[0], 3),
        "max": round(ordered[-1], 3),
        "stdev": round(statistics.stdev(ordered), 3) if len(ordered) > 1 else 0.0,
    }


def tree_size(root: pathlib.Path) -> dict[str, int]:
    files = 0
    total = 0
    if root.exists():
        for dirpath, _, filenames in os.walk(root):
            for filename in filenames:
                try:
                    total += os.lstat(os.path.join(dirpath, filename)).st_size
                    files += 1
                except OSError:
                    pass
    return {"files": files, "bytes": total}


def frost_state_sizes(root: pathlib.Path) -> dict[str, Any]:
    state = root / ".frost"
    sizes: dict[str, Any] = {"total": tree_size(state)}
    for name in ("journal.bin", "hashcache.bin", "toolchain.bin"):
        path = state / name
        sizes[name] = path.stat().st_size if path.exists() else 0
    sizes["graph_stores"] = sum(p.stat().st_size for p in state.glob("graph-*.bin"))
    sizes["cas"] = tree_size(state / "cas")
    return sizes


class Frost:
    def __init__(self, executable: str, root: pathlib.Path, jobs: int | None) -> None:
        self.executable = executable
        self.root = root
        self.jobs = jobs
        self.env = dict(os.environ)
        # A user's ~/.config/frost/frostrc must not take part in a measurement.
        self.env["XDG_CONFIG_HOME"] = str(root.parent / ".frost-scale-no-user-config")

    def argv(self, *args: str) -> list[str]:
        return [self.executable, "-C", str(self.root), *args]

    def run(self, *args: str, check: bool = True, timeout_s: float = 3600) -> Run:
        result = run_measured(self.argv(*args), env=self.env, timeout_s=timeout_s)
        if check and result.code != 0:
            raise RuntimeError(
                f"frost {' '.join(args)} exited {result.code}\n{result.stdout[-4000:]}"
                f"{result.stderr[-4000:]}"
            )
        return result

    def build(
        self, *extra: str, check: bool = True, events: bool = True
    ) -> tuple[Run, dict[str, int]]:
        """Build and count what ran.

        `events=False` counts from the summary line instead of the event
        stream. A daemon asked for a stream runs a child build rather than
        answering from its certificate, so a daemon no-op is timed without one.
        """
        stream = self.root.parent / f"events-{os.getpid()}.ndjson"
        stream.unlink(missing_ok=True)
        args = [f"--build-event-json={stream}"] if events else []
        args += ["build", "--no-tui"]
        if self.jobs:
            args += ["-j", str(self.jobs)]
        result = self.run(*args, *extra, check=check)
        if stream.exists():
            counts = count_events(stream)
            stream.unlink()
        else:
            # Not asked for, or a frost from before 0.14, which wrote none when
            # the certificate or the daemon answered: the summary says what ran.
            counts = count_summary(result.stdout)
        return result, counts


def count_events(path: pathlib.Path) -> dict[str, int]:
    counts = {"executed": 0, "cached": 0, "failed": 0, "total": 0, "all_cached": 0}
    if not path.exists():
        return counts
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("event") == "action_finished":
            counts["total"] += 1
            result = event.get("result")
            if result in ("executed", "flaky"):
                counts["executed"] += 1
            elif result == "failed":
                counts["failed"] += 1
            else:
                counts["cached"] += 1
        elif event.get("event") == "all_cached":
            counts["all_cached"] = int(event.get("actions", 0))
    return counts


def count_summary(stdout: str) -> dict[str, int]:
    built = re.search(r"(\d+) built", stdout)
    cached = re.search(r"(\d+) cached", stdout)
    total = re.search(r"(\d+) actions", stdout)
    executed = int(built.group(1)) if built else 0
    return {
        "executed": executed,
        "cached": int(cached.group(1)) if cached else 0,
        "failed": 0,
        "total": int(total.group(1)) if total else 0,
        "all_cached": int(total.group(1)) if total and "up to date" in stdout else 0,
    }


def copy_sources(source: pathlib.Path, destination: pathlib.Path, model: WorkspaceModel) -> None:
    """Copy the workspace minus everything a build writes."""
    outputs = set(model.declared_outputs())
    out_dirs = model.declared_output_dirs()

    def ignore(directory: str, names: list[str]) -> set[str]:
        rel_dir = pathlib.Path(directory).relative_to(source).as_posix()
        ignored = set()
        for name in names:
            rel = name if rel_dir == "." else f"{rel_dir}/{name}"
            owned = any(rel == d or rel.startswith(d + "/") for d in out_dirs)
            if name == ".frost" or rel in outputs or owned:
                ignored.add(name)
        return ignored

    shutil.copytree(source, destination, ignore=ignore, symlinks=True)


def compare_outputs(
    model: WorkspaceModel, left: pathlib.Path, right: pathlib.Path
) -> dict[str, Any]:
    """Byte-compare every declared output file and owned output tree."""
    mismatches: list[str] = []
    compared = 0
    for rel in model.declared_outputs():
        compared += 1
        a, b = left / rel, right / rel
        if not a.is_file() or not b.is_file() or a.read_bytes() != b.read_bytes():
            mismatches.append(rel)
    for rel in model.declared_output_dirs():
        a_files = {p.relative_to(left / rel) for p in (left / rel).rglob("*") if p.is_file()}
        b_files = {p.relative_to(right / rel) for p in (right / rel).rglob("*") if p.is_file()}
        if a_files != b_files:
            mismatches.append(rel + "/")
            continue
        for sub in sorted(a_files):
            compared += 1
            if (left / rel / sub).read_bytes() != (right / rel / sub).read_bytes():
                mismatches.append(f"{rel}/{sub}")
    return {"compared_files": compared, "mismatches": mismatches[:20], "ok": not mismatches}


def clean_oracle(
    frost: str, root: pathlib.Path, model: WorkspaceModel, scratch: pathlib.Path, jobs: int | None
) -> dict[str, Any]:
    """Build the same sources from nothing and compare every output."""
    reference = scratch / f"oracle-{time.monotonic_ns()}"
    copy_sources(root, reference, model)
    oracle = Frost(frost, reference, jobs)
    started = time.perf_counter()
    oracle.build()
    elapsed = (time.perf_counter() - started) * 1000.0
    result = compare_outputs(model, root, reference)
    result["clean_build_ms"] = round(elapsed, 3)
    shutil.rmtree(reference, ignore_errors=True)
    return result


def predicted_leaf_output(root: pathlib.Path, target: TargetModel) -> str | None:
    if target.reads is None:
        return None
    data = b"".join((root / rel).read_bytes() for rel in target.reads)
    return posix_cksum(data)


def edit_file(path: pathlib.Path, marker: str) -> None:
    path.write_text(f"{path.stem} edit={marker}\n", encoding="utf-8")


# ---------------------------------------------------------------------------
# scale


def run_scale(args: argparse.Namespace) -> dict[str, Any]:
    shape = shape_from_args(args)
    frost_path = shutil.which(args.frost) or args.frost
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="frost-scale-", dir=args.scratch))
    root = scratch / "workspace"
    env_before = environment()
    try:
        started = time.perf_counter()
        model = generate(root, shape)
        generate_ms = (time.perf_counter() - started) * 1000.0
        frost = Frost(frost_path, root, args.jobs)
        version = frost.run("--version").stdout.strip()

        # Cold plan: manifests discovered and the graph compiled from nothing.
        cold_plan = []
        cold_plan_rss = []
        for _ in range(args.cold_iterations):
            shutil.rmtree(root / ".frost", ignore_errors=True)
            result = frost.run("plan")
            cold_plan.append(result.elapsed_ms)
            cold_plan_rss.append(result.max_rss_kib)
        summary = re.search(r"\((\d+) actions\)", result.stdout)
        planned = int(summary.group(1)) if summary else None
        if planned != model.actions:
            raise RuntimeError(f"frost plan counted {planned} actions, the model {model.actions}")
        warm_plan = [frost.run("plan").elapsed_ms for _ in range(args.iterations)]

        shutil.rmtree(root / ".frost", ignore_errors=True)
        cold, cold_counts = frost.build()
        if cold_counts["executed"] != model.actions:
            raise RuntimeError(
                f"cold build executed {cold_counts['executed']} actions, model has {model.actions}"
            )

        # What a recursive watcher must cover: every directory, `.frost/`
        # included, once a build has populated it.
        directories = sum(1 for _ in os.walk(root))

        noop = []
        noop_rss = []
        for _ in range(args.iterations):
            result, counts = frost.build()
            if counts["executed"]:
                raise RuntimeError(f"warm no-op executed {counts['executed']} actions")
            noop.append(result.elapsed_ms)
            noop_rss.append(result.max_rss_kib)

        rng = random.Random(shape.seed + 1)
        leaf = []
        leaf_rss = []
        leaf_actions = []
        for iteration in range(args.iterations):
            source = rng.choice(model.leaf_sources)
            edit_file(root / source, f"leaf-{iteration}")
            expected = model.affected_actions([source])
            result, counts = frost.build()
            if counts["executed"] != expected:
                raise RuntimeError(
                    f"editing {source} executed {counts['executed']} actions, expected {expected}"
                )
            leaf.append(result.elapsed_ms)
            leaf_rss.append(result.max_rss_kib)
            leaf_actions.append(expected)

        header = None
        if model.header_inputs:
            samples = []
            actions = []
            for iteration in range(args.iterations):
                source = rng.choice(model.header_inputs)
                edit_file(root / source, f"header-{iteration}")
                expected = model.affected_actions([source])
                result, counts = frost.build()
                if counts["executed"] != expected:
                    raise RuntimeError(
                        f"editing {source} executed {counts['executed']} actions, "
                        f"expected {expected}"
                    )
                samples.append(result.elapsed_ms)
                actions.append(expected)
            header = {"ms": summarize(samples), "executed_actions": actions}

        daemon = measure_daemon(frost, model, rng, args.iterations)
        sizes = frost_state_sizes(root)
        oracle = clean_oracle(frost_path, root, model, scratch, args.jobs) if args.verify else None
        if oracle is not None and not oracle["ok"]:
            raise RuntimeError(f"outputs differ from a clean build: {oracle['mismatches']}")

        return {
            "schema": SCALE_SCHEMA,
            "generated_at": utc_now(),
            "reproduce": reproduce_command(args),
            "frost_executable": frost_path,
            "frost_version": version,
            "shape": dataclasses.asdict(shape),
            "workspace": {
                "targets": len(model.targets),
                "actions": model.actions,
                "planned_actions": planned,
                "source_files": model.files,
                "source_bytes": model.bytes,
                "manifests": shape.packages + 1,
                "declared_output_files": len(model.declared_outputs()),
                "owned_output_dirs": len(model.declared_output_dirs()),
                "owned_output_dir_files": shape.output_dir_targets * shape.output_dir_files,
                "directories_after_build": directories,
                "generate_ms": round(generate_ms, 3),
                "tree_digest": tree_digest(root) if args.digest else None,
            },
            "jobs": args.jobs,
            "iterations": args.iterations,
            "environment_before": env_before,
            "environment_after": environment(),
            "cold_plan_ms": summarize(cold_plan),
            "cold_plan_max_rss_kib": max(cold_plan_rss),
            "warm_plan_ms": summarize(warm_plan),
            "cold_build": {
                "ms": round(cold.elapsed_ms, 3),
                "max_rss_kib": cold.max_rss_kib,
                "executed_actions": cold_counts["executed"],
            },
            "warm_noop_ms": summarize(noop),
            "warm_noop_max_rss_kib": max(noop_rss),
            "leaf_change": {
                "ms": summarize(leaf),
                "max_rss_kib": max(leaf_rss),
                "executed_actions": leaf_actions,
            },
            "header_change": header,
            "daemon": daemon,
            "frost_state": sizes,
            "clean_build_oracle": oracle,
        }
    finally:
        if not args.keep:
            shutil.rmtree(scratch, ignore_errors=True)


def process_status(pid: int) -> dict[str, int] | None:
    """fd count and RSS/peak RSS of a live Linux process, or None."""
    try:
        fds = len(os.listdir(f"/proc/{pid}/fd"))
        values = {}
        for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
            key, _, rest = line.partition(":")
            if key in ("VmRSS", "VmHWM", "Threads"):
                values[key] = int(rest.split()[0])
        return {
            "fds": fds,
            "rss_kib": values.get("VmRSS", 0),
            "hwm_kib": values.get("VmHWM", 0),
            "threads": values.get("Threads", 0),
        }
    except (OSError, ValueError):
        return None


def wait_for_daemon(frost: Frost, timeout_s: float = 30) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        result = frost.run("daemon", "status", "--json", check=False)
        if result.code == 0 and json.loads(result.stdout or "{}").get("state") == "running":
            return
        time.sleep(0.1)
    raise RuntimeError("frost daemon did not become ready")


def measure_daemon(
    frost: Frost, model: WorkspaceModel, rng: random.Random, iterations: int
) -> dict[str, Any]:
    log = open(frost.root.parent / "daemon.log", "wb")
    daemon = subprocess.Popen(
        frost.argv("daemon", "serve"), env=frost.env, stdout=log, stderr=log
    )
    try:
        wait_for_daemon(frost)
        frost.build("--daemon")  # load the graph into the resident process
        noop = []
        for _ in range(iterations):
            result, counts = frost.build("--daemon", events=False)
            if counts["executed"] or "up to date" not in result.stdout:
                raise RuntimeError(f"daemon no-op executed {counts['executed']} actions")
            noop.append(result.elapsed_ms)
        leaf = []
        for iteration in range(iterations):
            source = rng.choice(model.leaf_sources)
            edit_file(frost.root / source, f"daemon-leaf-{iteration}")
            expected = model.affected_actions([source])
            result, counts = frost.build("--daemon")
            if counts["executed"] != expected:
                raise RuntimeError(
                    f"daemon build of {source} executed {counts['executed']}, expected {expected}"
                )
            leaf.append(result.elapsed_ms)
        status = process_status(daemon.pid)
        return {
            "noop_ms": summarize(noop),
            "leaf_change_ms": summarize(leaf),
            "resident": status,
        }
    finally:
        frost.run("daemon", "stop", check=False)
        try:
            daemon.wait(timeout=30)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        log.close()


def reproduce_command(args: argparse.Namespace) -> str:
    argv = [a for a in sys.argv[1:] if not a.startswith("--out")]
    return "python3 scripts/frost_scale.py " + " ".join(argv)


# ---------------------------------------------------------------------------
# soak


def analyse_leaks(
    samples: list[dict[str, Any]], *, fd_slack: int, rss_growth: float, rss_slack_kib: int
) -> dict[str, Any]:
    """Compare the warmed-up start of a series with its end.

    The first tenth is warm-up (lazy allocation, caches filling). A leak is a
    trend that survives it: more fds at the end than the warmed baseline plus
    `fd_slack`, or an end-of-run median RSS above `rss_growth` times the
    baseline median plus `rss_slack_kib`.
    """
    live = [s for s in samples if s is not None]
    if len(live) < 10:
        return {"ok": False, "reason": f"only {len(live)} samples"}
    tenth = max(1, len(live) // 10)
    baseline = live[tenth : 2 * tenth]
    tail = live[-tenth:]
    fd_base = max(s["fds"] for s in baseline)
    fd_end = max(s["fds"] for s in tail)
    rss_base = statistics.median(s["rss_kib"] for s in baseline)
    rss_end = statistics.median(s["rss_kib"] for s in tail)
    fd_ok = fd_end <= fd_base + fd_slack
    rss_ok = rss_end <= rss_base * rss_growth + rss_slack_kib
    return {
        "ok": fd_ok and rss_ok,
        "fd_baseline": fd_base,
        "fd_end": fd_end,
        "fd_max": max(s["fds"] for s in live),
        "rss_baseline_kib": rss_base,
        "rss_end_kib": rss_end,
        "rss_max_kib": max(s["rss_kib"] for s in live),
        "threads_max": max(s.get("threads", 0) for s in live),
    }


def count_lines(path: pathlib.Path) -> int:
    try:
        return path.read_text(encoding="utf-8").count("\n")
    except OSError:
        return 0


def run_soak(args: argparse.Namespace) -> dict[str, Any]:
    if not pathlib.Path("/proc/self/status").exists():
        raise RuntimeError("soak samples /proc and runs on Linux only")
    shape = shape_from_args(args)
    frost_path = shutil.which(args.frost) or args.frost
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="frost-soak-", dir=args.scratch))
    root = scratch / "workspace"
    env_before = environment()
    daemon = watch = None
    logs = []
    try:
        model = generate(root, shape)
        frost = Frost(frost_path, root, args.jobs)
        version = frost.run("--version").stdout.strip()
        initial, _ = frost.build()

        daemon_log = open(scratch / "daemon.log", "wb")
        logs.append(daemon_log)
        daemon = subprocess.Popen(
            frost.argv("daemon", "serve"), env=frost.env, stdout=daemon_log, stderr=daemon_log
        )
        wait_for_daemon(frost)
        frost.build("--daemon")

        marker = scratch / "watch-builds.log"
        watch_log = open(scratch / "watch.log", "wb")
        logs.append(watch_log)
        watch_argv = frost.argv("watch", "--debounce-ms", str(args.debounce_ms))
        if args.jobs:
            watch_argv += ["-j", str(args.jobs)]
        watch_argv += ["--run", "sh", "-c", f"echo built >> '{marker}'"]
        watch = subprocess.Popen(
            watch_argv, env=frost.env, stdout=watch_log, stderr=subprocess.STDOUT
        )
        deadline = time.monotonic() + 600
        while count_lines(marker) < 1:
            if watch.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError("frost watch never finished its initial build")
            time.sleep(0.05)
        time.sleep(args.debounce_ms / 1000 * 4)

        rng = random.Random(args.seed if args.seed is not None else shape.seed)
        started = time.monotonic()
        iterations = 0
        samples_daemon: list[Any] = []
        samples_watch: list[Any] = []
        journal_sizes: list[int] = []
        state_sizes: list[int] = []
        watch_ms: list[float] = []
        daemon_ms: list[float] = []
        daemon_executed: list[int] = []
        checkpoints: list[dict[str, Any]] = []
        verified_leaves = 0
        edit_kinds: dict[str, int] = {}
        # Choose the kind first: leaves outnumber the others a hundredfold, and
        # a uniform pick over files would almost never edit a header input.
        editable = [
            (weight, kind, paths)
            for weight, kind, paths in (
                (6, "leaf", model.leaf_sources),
                (2, "header", model.header_inputs),
                (2, "tree", model.tree_inputs),
            )
            if paths
        ]

        def pick() -> tuple[str, str]:
            weights = [weight for weight, _, _ in editable]
            _, kind, paths = rng.choices(editable, weights=weights)[0]
            return kind, rng.choice(paths)

        by_source = {}
        for target in model.targets.values():
            for source in target.sources:
                by_source.setdefault(source, []).append(target)
        history: dict[str, list[bytes]] = {}

        while time.monotonic() - started < args.duration and (
            args.max_iterations is None or iterations < args.max_iterations
        ):
            iterations += 1
            count = rng.choice((1, 1, 1, 2, 3))
            chosen = [pick() for _ in range(count)]
            for kind, rel in chosen:
                path = root / rel
                before = path.read_bytes()
                history.setdefault(rel, []).append(before)
                action = rng.random()
                if action < 0.15 and len(history[rel]) > 1:
                    # ABA: back to an earlier content, which a cache may serve.
                    path.write_bytes(rng.choice(history[rel][:-1]))
                    kind = f"{kind}-revert"
                elif action < 0.25:
                    # Touch without a content change: nothing may rerun.
                    os.utime(path)
                    kind = f"{kind}-touch"
                else:
                    edit_file(path, f"{iterations}-{rng.randrange(1 << 30)}")
                edit_kinds[kind] = edit_kinds.get(kind, 0) + 1

            previous = count_lines(marker)
            edit_started = time.perf_counter()
            deadline = time.monotonic() + args.build_timeout
            while count_lines(marker) <= previous:
                if watch.poll() is not None:
                    raise RuntimeError("frost watch exited during the soak")
                if time.monotonic() > deadline:
                    raise RuntimeError(
                        f"iteration {iterations}: watch did not finish a successful build "
                        f"within {args.build_timeout}s after editing {chosen}"
                    )
                time.sleep(0.02)
            watch_ms.append((time.perf_counter() - edit_started) * 1000.0)
            # Let any trailing event batch drain before the daemon builds, so
            # the two never write one workspace at the same time.
            while True:
                settled = count_lines(marker)
                time.sleep(args.debounce_ms / 1000 * 4)
                if count_lines(marker) == settled:
                    break

            for _, rel in chosen:
                for target in by_source.get(rel, []):
                    expected = predicted_leaf_output(root, target)
                    if expected is None:
                        continue
                    actual = (root / target.outputs[0]).read_text(encoding="utf-8")
                    if actual != expected:
                        raise RuntimeError(
                            f"iteration {iterations}: {target.outputs[0]} is {actual!r}, "
                            f"cksum of its inputs is {expected!r}"
                        )
                    verified_leaves += 1

            result, counts = frost.build("--daemon")
            daemon_ms.append(result.elapsed_ms)
            daemon_executed.append(counts["executed"])

            samples_daemon.append(process_status(daemon.pid))
            samples_watch.append(process_status(watch.pid))
            journal = root / ".frost" / "journal.bin"
            journal_sizes.append(journal.stat().st_size if journal.exists() else 0)
            state_sizes.append(tree_size(root / ".frost")["bytes"])

            if args.checkpoint_every and iterations % args.checkpoint_every == 0:
                checkpoint = clean_oracle(frost_path, root, model, scratch, args.jobs)
                checkpoint["iteration"] = iterations
                checkpoints.append(checkpoint)
                if not checkpoint["ok"]:
                    raise RuntimeError(f"checkpoint {iterations}: {checkpoint['mismatches']}")

        elapsed_s = time.monotonic() - started
        watch.send_signal(signal.SIGINT)
        try:
            watch.wait(timeout=30)
        except subprocess.TimeoutExpired:
            watch.kill()
            watch.wait()
        final = clean_oracle(frost_path, root, model, scratch, args.jobs)
        final["iteration"] = iterations
        checkpoints.append(final)

        daemon_leaks = analyse_leaks(
            samples_daemon,
            fd_slack=args.fd_slack,
            rss_growth=args.rss_growth,
            rss_slack_kib=args.rss_slack_mib * 1024,
        )
        watch_leaks = analyse_leaks(
            samples_watch,
            fd_slack=args.fd_slack,
            rss_growth=args.rss_growth,
            rss_slack_kib=args.rss_slack_mib * 1024,
        )
        journal_ok = max(journal_sizes, default=0) <= JOURNAL_COMPACTION_BYTES * 2
        growth = (
            (state_sizes[-1] - state_sizes[len(state_sizes) // 10]) / max(1, iterations)
            if state_sizes
            else 0
        )
        ok = (
            daemon_leaks["ok"]
            and watch_leaks["ok"]
            and journal_ok
            and all(c["ok"] for c in checkpoints)
        )
        return {
            "schema": SOAK_SCHEMA,
            "generated_at": utc_now(),
            "reproduce": reproduce_command(args),
            "frost_executable": frost_path,
            "frost_version": version,
            "shape": dataclasses.asdict(shape),
            "workspace": {
                "targets": len(model.targets),
                "actions": model.actions,
                "source_files": model.files,
            },
            "environment_before": env_before,
            "environment_after": environment(),
            "initial_build_ms": round(initial.elapsed_ms, 3),
            "duration_s": round(elapsed_s, 3),
            "iterations": iterations,
            "edit_kinds": edit_kinds,
            "verified_leaf_outputs": verified_leaves,
            "watch_edit_to_success_ms": summarize(watch_ms) if watch_ms else None,
            "daemon_build_after_watch_ms": summarize(daemon_ms) if daemon_ms else None,
            "daemon_build_executed_actions": {
                "zero": sum(1 for n in daemon_executed if n == 0),
                "nonzero": sum(1 for n in daemon_executed if n),
                "max": max(daemon_executed, default=0),
            },
            "daemon_process": daemon_leaks,
            "watch_process": watch_leaks,
            "journal_bytes": {
                "first": journal_sizes[0] if journal_sizes else 0,
                "max": max(journal_sizes, default=0),
                "last": journal_sizes[-1] if journal_sizes else 0,
                "compaction_threshold": JOURNAL_COMPACTION_BYTES,
                "bounded": journal_ok,
            },
            "frost_state_bytes": {
                "first": state_sizes[0] if state_sizes else 0,
                "last": state_sizes[-1] if state_sizes else 0,
                "growth_per_iteration": round(growth, 1),
            },
            "checkpoints": checkpoints,
            "ok": ok,
        }
    finally:
        for process in (watch,):
            if process is not None and process.poll() is None:
                process.kill()
                process.wait()
        if daemon is not None:
            try:
                Frost(frost_path, root, None).run("daemon", "stop", check=False, timeout_s=30)
                daemon.wait(timeout=30)
            except (subprocess.TimeoutExpired, RuntimeError, OSError):
                daemon.kill()
                daemon.wait()
        for log in logs:
            log.close()
        if args.keep:
            print(f"kept {scratch}", file=sys.stderr)
        else:
            shutil.rmtree(scratch, ignore_errors=True)


# ---------------------------------------------------------------------------
# CLI


def add_shape_arguments(parser: argparse.ArgumentParser, default: str) -> None:
    parser.add_argument("--shape", choices=sorted(PRESETS), default=default)
    for field in dataclasses.fields(Shape):
        if field.name in ("name",):
            continue
        parser.add_argument(
            "--" + field.name.replace("_", "-"),
            dest=field.name,
            type=int,
            default=None,
            help=f"override the preset's {field.name}",
        )


def write_report(report: dict[str, Any], out: str | None) -> None:
    text = json.dumps(report, indent=2, sort_keys=False) + "\n"
    if out:
        pathlib.Path(out).write_text(text, encoding="utf-8")
    sys.stdout.write(text)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    gen = sub.add_parser("generate", help="write a synthetic workspace")
    add_shape_arguments(gen, "wide")
    gen.add_argument("--out", required=True, help="empty or missing directory")

    digest = sub.add_parser("digest", help="print the tree digest of a workspace")
    digest.add_argument("root")

    scale = sub.add_parser("scale", help="measure one shape")
    add_shape_arguments(scale, "monorepo")
    scale.add_argument("--frost", default="frost")
    scale.add_argument("--jobs", type=int, default=None)
    scale.add_argument("--iterations", type=int, default=5)
    scale.add_argument("--cold-iterations", type=int, default=1)
    scale.add_argument("--no-verify", dest="verify", action="store_false")
    scale.add_argument("--digest", action="store_true", help="record the generated tree digest")
    scale.add_argument("--scratch", default=None)
    scale.add_argument("--keep", action="store_true")
    scale.add_argument("--out", default=None)

    soak = sub.add_parser("soak", help="daemon + watch under a stream of edits (Linux)")
    add_shape_arguments(soak, "soak")
    soak.add_argument("--frost", default="frost")
    soak.add_argument("--jobs", type=int, default=None)
    soak.add_argument("--duration", type=float, default=300.0, help="seconds")
    soak.add_argument("--max-iterations", type=int, default=None)
    soak.add_argument("--debounce-ms", type=int, default=100)
    soak.add_argument("--build-timeout", type=float, default=300.0)
    soak.add_argument("--checkpoint-every", type=int, default=50)
    soak.add_argument("--fd-slack", type=int, default=8)
    soak.add_argument("--rss-growth", type=float, default=1.5)
    soak.add_argument("--rss-slack-mib", type=int, default=32)
    soak.add_argument("--scratch", default=None)
    soak.add_argument("--keep", action="store_true")
    soak.add_argument("--out", default=None)

    args = parser.parse_args(argv)
    if args.command == "generate":
        model = generate(pathlib.Path(args.out), shape_from_args(args))
        print(
            json.dumps(
                {
                    "shape": dataclasses.asdict(model.shape),
                    "targets": len(model.targets),
                    "actions": model.actions,
                    "files": model.files,
                    "bytes": model.bytes,
                    "tree_digest": tree_digest(pathlib.Path(args.out)),
                },
                indent=2,
            )
        )
        return 0
    if args.command == "digest":
        print(tree_digest(pathlib.Path(args.root)))
        return 0
    runner, schema = (run_scale, SCALE_SCHEMA) if args.command == "scale" else (run_soak, SOAK_SCHEMA)
    try:
        report = runner(args)
    except Exception as error:  # noqa: BLE001 - the failure is the evidence
        # Where a run breaks is the finding, so it is written down in the
        # same place a success would have been rather than only on stderr.
        report = {
            "schema": schema,
            "generated_at": utc_now(),
            "reproduce": reproduce_command(args),
            "shape": dataclasses.asdict(shape_from_args(args)),
            "environment": environment(),
            "ok": False,
            "error": f"{type(error).__name__}: {error}",
        }
    report.setdefault("ok", True)
    write_report(report, args.out)
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
