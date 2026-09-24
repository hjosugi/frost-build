#!/usr/bin/env python3
"""Measure what a persistent compiler worker saves for javac and tsc (#145).

The question is narrow: for one module-sized compile after a one-file edit,
how much of a cold compiler invocation is process start and compiler warm-up,
and how much of that does a long-lived worker remove? Each scenario compiles
the same generated sources to its own output directory; after every compile
the output tree is digested and compared with the cold compile of the same
edit, so a faster worker that produced different bytes cannot pass as a win.

Two hazard experiments then show worker state outliving its inputs, and
whether a cold rerun -- the shape of `--check-determinism` -- catches it.

Reproduce (see docs/31_persistent_workers.md for the toolchain pins):

    npm install --prefix /tmp/ts6 typescript@6.0.3
    npm install --prefix /tmp/ts7 typescript@7.0.2
    python3 scripts/bench_persistent_worker.py \\
        --typescript-js /tmp/ts6/node_modules/typescript \\
        --tsc-native /tmp/ts7/node_modules/@typescript/typescript-linux-x64/lib/tsc \\
        --out bench/baselines/2026-09-24-issue-145-persistent-worker.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import resource
import shutil
import statistics
import subprocess
import tempfile
import time
import zipfile
from datetime import datetime, timezone
from typing import Callable

SCHEMA = "frost-persistent-worker-bench-v1"
REPO = pathlib.Path(__file__).resolve().parent.parent
FIXTURE = REPO / "bench" / "fixtures" / "persistent-worker"

# The one-file edit every iteration makes. Both spellings are the same length
# on purpose: the stat-keyed hazard below depends on an edit that changes
# content without changing size.
LEAF_VALUES = ("1", "2")

# Fixed zip timestamp so the generated class-path jars are byte-stable.
ZIP_EPOCH = (1980, 1, 1, 0, 0, 0)


# --------------------------------------------------------------------------
# Deterministic fixtures
# --------------------------------------------------------------------------


def java_unit_name(index: int) -> str:
    return f"Unit{index:03d}"


def java_sources(units: int, variant: int) -> dict[str, str]:
    """A dependency chain of `units` classes plus a main class.

    Unit000 is the leaf the edit loop changes; every other unit calls the one
    before it, so the whole module is recompiled and type-checked each time,
    which is what a module-granular javac action does.
    """
    if units < 2:
        raise ValueError("units must be at least 2")
    leaf = LEAF_VALUES[variant % len(LEAF_VALUES)]
    files: dict[str, str] = {}
    for index in range(units):
        name = java_unit_name(index)
        if index == 0:
            previous = f"x + LEAF"
            constant = f"    public static final int LEAF = {leaf};\n"
        else:
            previous = f"{java_unit_name(index - 1)}.fold(x)"
            constant = ""
        files[f"src/bench/{name}.java"] = (
            "package bench;\n\n"
            "import java.util.List;\n"
            "import java.util.Map;\n"
            "import java.util.function.Function;\n"
            "import java.util.stream.Collectors;\n"
            "import java.util.stream.IntStream;\n\n"
            f"public final class {name} {{\n"
            f"{constant}"
            f"    public static final int SEED = {index + 3};\n\n"
            f"    public record Item(int id, String name, List<Integer> values) {{}}\n\n"
            f"    public interface Shape<T extends Comparable<T>> {{\n"
            f"        T key();\n\n"
            f"        default <R> R apply(Function<? super T, ? extends R> f) {{\n"
            f"            return f.apply(key());\n"
            f"        }}\n"
            f"    }}\n\n"
            f"    private {name}() {{}}\n\n"
            f"    public static List<Item> items(int n) {{\n"
            f"        return IntStream.range(0, n)\n"
            f"                .mapToObj(i -> new Item(i, \"u{index}-\" + i, List.of(i, i * SEED)))\n"
            f"                .collect(Collectors.toList());\n"
            f"    }}\n\n"
            f"    public static Map<Integer, String> index(List<Item> items) {{\n"
            f"        return items.stream().collect(Collectors.toMap(Item::id, Item::name));\n"
            f"    }}\n\n"
            f"    public static int fold(int x) {{\n"
            f"        int local = items(3).stream()\n"
            f"                .mapToInt(item -> item.values().stream().mapToInt(Integer::intValue).sum())\n"
            f"                .sum();\n"
            f"        Shape<String> shape = () -> \"k\" + local;\n"
            f"        return {previous} * 31 + local + shape.apply(String::length);\n"
            f"    }}\n"
            f"}}\n"
        )
    last = java_unit_name(units - 1)
    files["src/bench/Main.java"] = (
        "package bench;\n\n"
        "public final class Main {\n"
        "    private Main() {}\n\n"
        "    public static void main(String[] args) {\n"
        f"        System.out.println({last}.fold(1));\n"
        "    }\n"
        "}\n"
    )
    return files


def ts_module_name(index: int) -> str:
    return f"m{index:03d}"


def typescript_sources(units: int, variant: int) -> dict[str, str]:
    """The TypeScript analogue of `java_sources`: an ESM import chain."""
    if units < 2:
        raise ValueError("units must be at least 2")
    leaf = LEAF_VALUES[variant % len(LEAF_VALUES)]
    files: dict[str, str] = {}
    for index in range(units):
        name = ts_module_name(index)
        if index == 0:
            header = f"export const LEAF = {leaf};\n"
            previous = "x + LEAF"
        else:
            header = f'import {{ fold as previous }} from "./{ts_module_name(index - 1)}.js";\n'
            previous = "previous(x)"
        files[f"src/{name}.ts"] = (
            f"{header}\n"
            f"export interface Item{index:03d} {{\n"
            f"  readonly id: number;\n"
            f"  readonly name: string;\n"
            f"  readonly values: readonly number[];\n"
            f"}}\n\n"
            f"export type Keyed{index:03d}<T> = {{ [K in keyof T as `u{index}_${{string & K}}`]: T[K] }};\n\n"
            f"export const SEED = {index + 3};\n\n"
            f"export function items(n: number): Item{index:03d}[] {{\n"
            f"  return Array.from({{ length: n }}, (_, i) => ({{ id: i, name: `u{index}-${{i}}`, values: [i, i * SEED] }}));\n"
            f"}}\n\n"
            f"export class Box{index:03d}<T extends {{ id: number }}> {{\n"
            f"  constructor(private readonly value: T) {{}}\n\n"
            f"  map<U extends {{ id: number }}>(f: (value: T) => U): Box{index:03d}<U> {{\n"
            f"    return new Box{index:03d}(f(this.value));\n"
            f"  }}\n\n"
            f"  get id(): number {{\n"
            f"    return this.value.id;\n"
            f"  }}\n"
            f"}}\n\n"
            f"export function fold(x: number): number {{\n"
            f"  const local = items(3).reduce((sum, item) => sum + item.values.reduce((a, b) => a + b, 0), 0);\n"
            f"  const keyed: Keyed{index:03d}<{{ a: number }}> = {{ u{index}_a: local }};\n"
            f"  return {previous} * 31 + keyed.u{index}_a + new Box{index:03d}({{ id: local }}).map((v) => ({{ id: v.id + 1 }})).id;\n"
            f"}}\n"
        )
    files["src/main.ts"] = (
        f'import {{ fold }} from "./{ts_module_name(units - 1)}.js";\n\n'
        "console.log(fold(1));\n"
    )
    files["package.json"] = '{ "private": true, "type": "module" }\n'
    return files


def tsconfig(out_dir: str) -> str:
    return json.dumps(
        {
            "compilerOptions": {
                "target": "ES2022",
                "module": "NodeNext",
                "moduleResolution": "NodeNext",
                "strict": True,
                "noEmitOnError": True,
                "types": [],
                "rootDir": "src",
                "outDir": out_dir,
            },
            "include": ["src"],
        },
        indent=2,
        sort_keys=True,
    ) + "\n"


def write_tree(root: pathlib.Path, files: dict[str, str]) -> None:
    for relative, text in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        # Only rewrite what changed, as an editor would: the stat-keyed hazard
        # depends on untouched files keeping their stat.
        if path.exists() and path.read_text(encoding="utf-8") == text:
            continue
        path.write_text(text, encoding="utf-8")


def tree_digest(root: pathlib.Path) -> str:
    """SHA-256 over sorted relative paths and file bytes."""
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        relative = path.relative_to(root).as_posix().encode()
        data = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big") + relative)
        digest.update(len(data).to_bytes(8, "big") + data)
    return digest.hexdigest()


def fresh_dir(path: pathlib.Path) -> pathlib.Path:
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True)
    return path


# --------------------------------------------------------------------------
# Statistics and ordering
# --------------------------------------------------------------------------


def percentile(sorted_samples: list[float], fraction: float) -> float:
    """Linear-interpolated percentile of already sorted samples."""
    if not sorted_samples:
        raise ValueError("no samples")
    position = (len(sorted_samples) - 1) * fraction
    lower = int(position)
    upper = min(lower + 1, len(sorted_samples) - 1)
    weight = position - lower
    return sorted_samples[lower] * (1 - weight) + sorted_samples[upper] * weight


def summarize(samples: list[float]) -> dict[str, object]:
    if not samples:
        raise ValueError("no samples")
    ordered = sorted(samples)
    median = statistics.median(ordered)
    return {
        "samples": [round(value, 3) for value in samples],
        "n": len(samples),
        "median": round(median, 3),
        "p10": round(percentile(ordered, 0.10), 3),
        "p90": round(percentile(ordered, 0.90), 3),
        "min": round(ordered[0], 3),
        "max": round(ordered[-1], 3),
        "mad": round(statistics.median(abs(value - median) for value in ordered), 3),
    }


def rotation(names: list[str], iteration: int) -> list[str]:
    """Rotate the scenario order every iteration so none always runs first."""
    if not names:
        return []
    shift = iteration % len(names)
    return names[shift:] + names[:shift]


# --------------------------------------------------------------------------
# Processes
# --------------------------------------------------------------------------


def run(command: list[str], *, cwd: pathlib.Path) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(command, cwd=cwd, text=True, capture_output=True)
    if completed.returncode != 0:
        raise RuntimeError(
            f"command failed ({completed.returncode}): {' '.join(command)}\n"
            f"{completed.stdout}{completed.stderr}"
        )
    return completed


def children_cpu_ms() -> float:
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return (usage.ru_utime + usage.ru_stime) * 1000


def timed(command: list[str], *, cwd: pathlib.Path) -> tuple[float, float, int]:
    """Wall and CPU milliseconds of one cold process, and its exit status.

    CPU is the child's user+system time from rusage, every thread included.
    Live workers are children too, but rusage only counts children that have
    been waited for, so the delta is this process alone.
    """
    cpu_before = children_cpu_ms()
    start = time.perf_counter()
    completed = subprocess.run(command, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    elapsed = (time.perf_counter() - start) * 1000
    return elapsed, children_cpu_ms() - cpu_before, completed.returncode


def first_line(command: list[str]) -> str:
    completed = subprocess.run(command, text=True, capture_output=True)
    return (completed.stdout + completed.stderr).strip().splitlines()[0]


class Worker:
    """A line-framed worker process: one request line in, one response out."""

    def __init__(self, command: list[str], *, cwd: pathlib.Path, stderr: pathlib.Path) -> None:
        self.stderr = stderr.open("w", encoding="utf-8")
        start = time.perf_counter()
        self.process = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            text=True,
            bufsize=1,
        )
        ready = self.process.stdout.readline()
        if not ready:
            raise RuntimeError(f"worker exited before ready: {' '.join(command)}")
        self.start_ms = (time.perf_counter() - start) * 1000
        self.ready = ready.strip()

    def request(self, line: str) -> tuple[float, str]:
        start = time.perf_counter()
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()
        response = self.process.stdout.readline()
        elapsed = (time.perf_counter() - start) * 1000
        if not response:
            raise RuntimeError("worker exited mid-request")
        return elapsed, response.strip()

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        self.process.wait(timeout=30)
        self.stderr.close()


def parse_javac_response(response: str) -> tuple[int, float, float]:
    """(exit, compile ms, process CPU ms) from one JavacWorker response."""
    exit_code, nanos, _diagnostic_bytes, cpu_nanos = response.split()
    return int(exit_code), int(nanos) / 1e6, int(cpu_nanos) / 1e6


def parse_ts_response(response: str) -> tuple[int, float, float, int]:
    """(exit, compile ms, process CPU ms, reused SourceFiles) from ts_worker."""
    decoded = json.loads(response)
    return (
        int(decoded["exit"]),
        decoded["compile_ns"] / 1e6,
        decoded["cpu_ns"] / 1e6,
        int(decoded["reused_source_files"]),
    )


# --------------------------------------------------------------------------
# javac
# --------------------------------------------------------------------------


def javac_argv(work: pathlib.Path, out: pathlib.Path, classpath: pathlib.Path | None = None) -> list[str]:
    sources = sorted(str(path) for path in (work / "src").rglob("*.java"))
    argv = ["-d", str(out), "-encoding", "UTF-8", "-implicit:none"]
    if classpath is not None:
        argv += ["-cp", str(classpath)]
    return argv + sources


def write_argfile(path: pathlib.Path, argv: list[str]) -> pathlib.Path:
    path.write_text("\n".join(argv) + "\n", encoding="utf-8")
    return path


def compile_java_worker(temporary: pathlib.Path) -> pathlib.Path:
    classes = fresh_dir(temporary / "worker-classes")
    run(["javac", "-d", str(classes), str(FIXTURE / "JavacWorker.java")], cwd=temporary)
    return classes


def bench_javac(temporary: pathlib.Path, *, units: int, iterations: int, warmup: int) -> dict[str, object]:
    classes = compile_java_worker(temporary)
    work = temporary / "javac"
    work.mkdir()
    workers = {
        "worker_fresh_context": Worker(
            ["java", "-cp", str(classes), "JavacWorker"], cwd=work, stderr=temporary / "javac-fresh.stderr"
        ),
        "worker_shared_file_manager": Worker(
            ["java", "-cp", str(classes), "JavacWorker", "--shared-file-manager"],
            cwd=work,
            stderr=temporary / "javac-shared.stderr",
        ),
    }
    scenarios = ["cold", *workers]
    wall: dict[str, list[float]] = {name: [] for name in scenarios}
    cpu: dict[str, list[float]] = {name: [] for name in scenarios}
    inner: dict[str, list[float]] = {name: [] for name in workers}
    mismatches: list[dict[str, object]] = []
    try:
        for iteration in range(iterations):
            write_tree(work, java_sources(units, iteration))
            digests: dict[str, str] = {}
            for name in rotation(scenarios, iteration):
                out = fresh_dir(temporary / f"javac-out-{name}")
                argv = javac_argv(work, out)
                if name == "cold":
                    elapsed, cpu_ms, code = timed(["javac", *argv], cwd=work)
                else:
                    argfile = write_argfile(temporary / f"javac-{name}.args", argv)
                    elapsed, response = workers[name].request(str(argfile))
                    code, compile_ms, cpu_ms = parse_javac_response(response)
                    inner[name].append(compile_ms)
                if code != 0:
                    raise RuntimeError(f"javac {name} failed on iteration {iteration}")
                wall[name].append(elapsed)
                cpu[name].append(cpu_ms)
                digests[name] = tree_digest(out)
            for name, value in digests.items():
                if value != digests["cold"]:
                    mismatches.append({"iteration": iteration, "scenario": name})
        # The per-action floor: one empty class, so almost all of a cold
        # compile is JVM start and javac class loading, and almost all of a
        # warm worker's is request handling. It prices a fine partition.
        trivial = temporary / "javac-trivial"
        write_tree(trivial, {"Empty.java": "final class Empty {}\n"})

        def trivial_javac(name: str) -> tuple[float, float, int]:
            out = fresh_dir(temporary / f"javac-trivial-out-{name}")
            argv = ["-d", str(out), str(trivial / "Empty.java")]
            if name == "cold":
                return timed(["javac", *argv], cwd=trivial)
            argfile = write_argfile(temporary / f"javac-trivial-{name}.args", argv)
            elapsed, response = workers[name].request(str(argfile))
            code, _, cpu_ms = parse_javac_response(response)
            return elapsed, cpu_ms, code

        floor = per_action_floor(["cold", "worker_fresh_context"], iterations, trivial_javac)
    finally:
        for worker in workers.values():
            worker.close()
    return toolchain_report(
        wall=wall,
        cpu=cpu,
        inner=inner,
        workers=workers,
        warmup=warmup,
        mismatches=mismatches,
        extra={"per_action_floor": floor},
    )


def per_action_floor(
    names: list[str], iterations: int, compile_once: Callable[[str], tuple[float, float, int]]
) -> dict[str, object]:
    """Wall and CPU of a trivial compile per scenario, in rotating order."""
    wall: dict[str, list[float]] = {name: [] for name in names}
    cpu: dict[str, list[float]] = {name: [] for name in names}
    for iteration in range(iterations):
        for name in rotation(names, iteration):
            elapsed, cpu_ms, code = compile_once(name)
            if code != 0:
                raise RuntimeError(f"trivial compile failed for {name}")
            wall[name].append(elapsed)
            cpu[name].append(cpu_ms)
    return {name: {"wall_ms": summarize(wall[name]), "cpu_ms": summarize(cpu[name])} for name in names}


def toolchain_report(
    *,
    wall: dict[str, list[float]],
    cpu: dict[str, list[float]],
    inner: dict[str, list[float]],
    workers: dict[str, Worker],
    warmup: int,
    mismatches: list[dict[str, object]],
    extra: dict[str, object],
) -> dict[str, object]:
    """Wall and CPU summaries per scenario, with the ratios docs/31 quotes.

    Wall time is what a developer waits for, but on a loaded host it is
    mostly queueing; CPU time is the work a worker actually removes and is
    far less sensitive to other tenants. Both are kept, raw.
    """
    cold = summarize(wall["cold"])
    cold_cpu = summarize(cpu["cold"])
    report: dict[str, object] = {"cold_compile_ms": cold, "cold_compile_cpu_ms": cold_cpu}
    for name, worker in workers.items():
        steady = summarize(wall[name][warmup:])
        steady_cpu = summarize(cpu[name][warmup:])
        report[name] = {
            "start_to_ready_ms": round(worker.start_ms, 3),
            "warmup_curve_roundtrip_ms": [round(value, 3) for value in wall[name][:warmup]],
            "warmup_curve_cpu_ms": [round(value, 3) for value in cpu[name][:warmup]],
            "warmup_curve_compile_ms": [round(value, 3) for value in inner[name][:warmup]],
            "steady_roundtrip_ms": steady,
            "steady_cpu_ms": steady_cpu,
            "steady_compile_ms": summarize(inner[name][warmup:]),
            "cold_over_steady_median": round(cold["median"] / steady["median"], 3),
            "cold_over_steady_cpu_median": round(cold_cpu["median"] / steady_cpu["median"], 3),
            "first_request_over_steady_median": round(wall[name][0] / steady["median"], 3),
            "first_request_over_steady_cpu_median": round(cpu[name][0] / steady_cpu["median"], 3),
        }
    report["outputs_identical_to_cold"] = not mismatches
    report["output_mismatches"] = mismatches
    report.update(extra)
    return report


# --------------------------------------------------------------------------
# tsc
# --------------------------------------------------------------------------


def bench_tsc(
    temporary: pathlib.Path,
    *,
    typescript_js: pathlib.Path,
    tsc_native: pathlib.Path | None,
    units: int,
    iterations: int,
    warmup: int,
) -> dict[str, object]:
    work = temporary / "tsc"
    work.mkdir()
    tsc_js = typescript_js / "bin" / "tsc"
    modes = {"worker_fresh_program": "fresh", "worker_reuse_program": "reuse-content"}
    for name in ["cold", *modes, "native"]:
        (work / f"tsconfig.{name}.json").write_text(tsconfig(f"out-{name}"), encoding="utf-8")
    workers = {
        name: Worker(
            ["node", str(FIXTURE / "ts_worker.mjs"), str(typescript_js), mode],
            cwd=work,
            stderr=temporary / f"tsc-{name}.stderr",
        )
        for name, mode in modes.items()
    }
    scenarios = ["cold", *workers]
    if tsc_native is not None:
        scenarios.append("native_cold")
    wall: dict[str, list[float]] = {name: [] for name in scenarios}
    cpu: dict[str, list[float]] = {name: [] for name in scenarios}
    inner: dict[str, list[float]] = {name: [] for name in workers}
    reused: dict[str, list[int]] = {name: [] for name in workers}
    mismatches: list[dict[str, object]] = []
    native_digests: list[str] = []
    try:
        for iteration in range(iterations):
            write_tree(work, typescript_sources(units, iteration))
            digests: dict[str, str] = {}
            for name in rotation(scenarios, iteration):
                config_name = "native" if name == "native_cold" else name
                out = fresh_dir(work / f"out-{config_name}")
                project = str(work / f"tsconfig.{config_name}.json")
                if name == "cold":
                    elapsed, cpu_ms, code = timed(["node", str(tsc_js), "-p", project], cwd=work)
                elif name == "native_cold":
                    elapsed, cpu_ms, code = timed([str(tsc_native), "-p", project], cwd=work)
                else:
                    elapsed, response = workers[name].request(json.dumps({"project": project}))
                    code, compile_ms, cpu_ms, reuse_count = parse_ts_response(response)
                    inner[name].append(compile_ms)
                    reused[name].append(reuse_count)
                if code != 0:
                    raise RuntimeError(f"tsc {name} failed on iteration {iteration}")
                wall[name].append(elapsed)
                cpu[name].append(cpu_ms)
                if name == "native_cold":
                    native_digests.append(tree_digest(out))
                else:
                    digests[name] = tree_digest(out)
            for name, value in digests.items():
                if value != digests["cold"]:
                    mismatches.append({"iteration": iteration, "scenario": name})
        trivial = temporary / "tsc-trivial"
        write_tree(
            trivial,
            {
                "src/empty.ts": "export {};\n",
                "package.json": '{ "private": true, "type": "module" }\n',
                "tsconfig.json": tsconfig("out"),
            },
        )
        trivial_project = str(trivial / "tsconfig.json")

        def trivial_tsc(name: str) -> tuple[float, float, int]:
            fresh_dir(trivial / "out")
            if name == "cold":
                return timed(["node", str(tsc_js), "-p", trivial_project], cwd=trivial)
            if name == "native_cold":
                return timed([str(tsc_native), "-p", trivial_project], cwd=trivial)
            elapsed, response = workers[name].request(json.dumps({"project": trivial_project}))
            code, _, cpu_ms, _ = parse_ts_response(response)
            return elapsed, cpu_ms, code

        floor = per_action_floor(scenarios, iterations, trivial_tsc)
    finally:
        for worker in workers.values():
            worker.close()
    extra: dict[str, object] = {"per_action_floor": floor}
    for name in workers:
        extra[name + "_reused_source_files"] = reused[name]
    report = toolchain_report(
        wall={name: wall[name] for name in ["cold", *workers]},
        cpu={name: cpu[name] for name in ["cold", *workers]},
        inner=inner,
        workers=workers,
        warmup=warmup,
        mismatches=mismatches,
        extra=extra,
    )
    if tsc_native is not None:
        # A different compiler: its bytes are compared only with each other, to
        # show the native reference is itself deterministic across edits.
        report["native_reference"] = {
            "cold_compile_ms": summarize(wall["native_cold"]),
            "cold_compile_cpu_ms": summarize(cpu["native_cold"]),
            "distinct_output_digests": len(set(native_digests)),
        }
    return report


# --------------------------------------------------------------------------
# Hazards
# --------------------------------------------------------------------------


def jar_bytes(classes: pathlib.Path) -> bytes:
    """A byte-stable jar of every class file under `classes`."""
    handle, name = tempfile.mkstemp(suffix=".jar")
    os.close(handle)
    buffer = pathlib.Path(name)
    try:
        with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_STORED) as archive:
            for path in sorted(classes.rglob("*.class")):
                info = zipfile.ZipInfo(path.relative_to(classes).as_posix(), ZIP_EPOCH)
                archive.writestr(info, path.read_bytes())
        return buffer.read_bytes()
    finally:
        buffer.unlink()


# Two library edits, each as (version 1, version 2) of `lib.Lib`'s body and of
# the client that uses it. `api` removes the method the old client called; the
# new client calls its replacement. `constant` changes an inlined compile-time
# constant without changing the jar's size or the client's source -- javac
# copies the value into the client's class file, so only the bytes can tell.
LIBRARY_EDITS = {
    "api": (
        ("    public static int one() {\n        return 1;\n    }\n", "return lib.Lib.one();"),
        ("    public static int two() {\n        return 1;\n    }\n", "return lib.Lib.two();"),
    ),
    "constant": (
        ("    public static final int VALUE = 1;\n", "return lib.Lib.VALUE;"),
        ("    public static final int VALUE = 2;\n", "return lib.Lib.VALUE;"),
    ),
}


def library_jar(temporary: pathlib.Path, tag: str, body: str) -> bytes:
    source = temporary / f"lib-{tag}" / "lib" / "Lib.java"
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text(
        "package lib;\n\npublic final class Lib {\n    private Lib() {}\n\n" + body + "}\n",
        encoding="utf-8",
    )
    classes = fresh_dir(temporary / f"lib-{tag}-classes")
    run(["javac", "-d", str(classes), str(source)], cwd=temporary)
    return jar_bytes(classes)


def client_source(statement: str) -> dict[str, str]:
    return {
        "src/client/Client.java": (
            "package client;\n\n"
            "public final class Client {\n"
            "    private Client() {}\n\n"
            "    public static int call() {\n"
            f"        {statement}\n"
            "    }\n"
            "}\n"
        )
    }


def publish_by_rename(path: pathlib.Path, data: bytes) -> None:
    staged = path.with_suffix(".staged")
    staged.write_bytes(data)
    os.replace(staged, path)


def publish_in_place(path: pathlib.Path, data: bytes) -> None:
    with path.open("r+b") as handle:
        handle.seek(0)
        handle.write(data)
        handle.truncate()


PUBLICATIONS = {
    "rename": ("class-path jar republished by atomic rename (new inode)", publish_by_rename),
    "in_place": ("class-path jar overwritten in place (same inode, same path)", publish_in_place),
}


def javac_classpath_hazard(temporary: pathlib.Path, classes: pathlib.Path) -> list[dict[str, object]]:
    """Republish a class-path jar between two requests to the same worker.

    A cold javac reads the path afresh. A worker whose file manager kept the
    old archive open either refuses the new client (`api`) or compiles the
    old constant into it (`constant`). Every combination of edit and
    publication gets fresh workers, so no case inherits another's state.
    """
    results = []
    for edit, (first, second) in LIBRARY_EDITS.items():
        jars = [library_jar(temporary, f"{edit}-{index}", body) for index, (body, _) in enumerate((first, second))]
        if edit == "constant" and len(jars[0]) != len(jars[1]):
            raise RuntimeError("the constant edit must not change the jar's size")
        for publication, (description, publish) in PUBLICATIONS.items():
            work = fresh_dir(temporary / f"hazard-javac-{edit}-{publication}")
            jar = work / "lib.jar"
            jar.write_bytes(jars[0])
            workers = {
                "worker_fresh_context": Worker(
                    ["java", "-cp", str(classes), "JavacWorker"], cwd=work, stderr=work / "fresh.stderr"
                ),
                "worker_shared_file_manager": Worker(
                    ["java", "-cp", str(classes), "JavacWorker", "--shared-file-manager"],
                    cwd=work,
                    stderr=work / "shared.stderr",
                ),
            }
            outcome: dict[str, object] = {}
            try:
                write_tree(work, client_source(first[1]))
                for name, worker in workers.items():
                    out = fresh_dir(work / f"out-first-{name}")
                    argfile = write_argfile(work / f"{name}.args", javac_argv(work, out, jar))
                    code, _, _ = parse_javac_response(worker.request(str(argfile))[1])
                    if code != 0:
                        raise RuntimeError(f"{name} failed the first, unchanged-jar request")
                publish(jar, jars[1])
                write_tree(work, client_source(second[1]))
                cold_out = fresh_dir(work / "out-cold")
                _, _, cold_code = timed(["javac", *javac_argv(work, cold_out, jar)], cwd=work)
                cold_digest = tree_digest(cold_out) if cold_code == 0 else None
                for name, worker in workers.items():
                    out = fresh_dir(work / f"out-second-{name}")
                    argfile = write_argfile(work / f"{name}.args", javac_argv(work, out, jar))
                    code, _, _ = parse_javac_response(worker.request(str(argfile))[1])
                    digest = tree_digest(out) if code == 0 else None
                    outcome[name] = {
                        "exit": code,
                        "matches_cold": code == cold_code and digest == cold_digest,
                    }
            finally:
                for worker in workers.values():
                    worker.close()
            results.append(
                {
                    "id": f"javac-classpath-{edit}-{publication}",
                    "toolchain": "javac",
                    "edit": f"{edit} change; {description}",
                    "cold_exit": cold_code,
                    "workers": outcome,
                }
            )
    return results


def tsc_stat_cache_hazard(temporary: pathlib.Path, typescript_js: pathlib.Path, units: int) -> dict[str, object]:
    """Edit a source without changing its size or mtime between two requests.

    That is what a same-length edit inside one timestamp tick looks like to
    anything keyed by stat. The content-keyed worker must follow the edit; the
    stat-keyed one is the shortcut being tested.
    """
    work = fresh_dir(temporary / "hazard-tsc")
    tsc_js = typescript_js / "bin" / "tsc"
    modes = {"worker_reuse_content_key": "reuse-content", "worker_reuse_mtime_key": "reuse-mtime"}
    for name in ["cold", *modes]:
        (work / f"tsconfig.{name}.json").write_text(tsconfig(f"out-{name}"), encoding="utf-8")
    write_tree(work, typescript_sources(units, 0))
    workers = {
        name: Worker(
            ["node", str(FIXTURE / "ts_worker.mjs"), str(typescript_js), mode],
            cwd=work,
            stderr=work / f"{name}.stderr",
        )
        for name, mode in modes.items()
    }
    outcome: dict[str, object] = {}
    try:
        for name, worker in workers.items():
            fresh_dir(work / f"out-{name}")
            code, _, _, _ = parse_ts_response(
                worker.request(json.dumps({"project": str(work / f"tsconfig.{name}.json")}))[1]
            )
            if code != 0:
                raise RuntimeError(f"{name} failed the first request")
        leaf = work / "src" / f"{ts_module_name(0)}.ts"
        before = leaf.stat()
        write_tree(work, typescript_sources(units, 1))
        os.utime(leaf, ns=(before.st_atime_ns, before.st_mtime_ns))
        after = leaf.stat()
        if (after.st_size, after.st_mtime_ns) != (before.st_size, before.st_mtime_ns):
            raise RuntimeError("the hazard edit changed the leaf's stat")
        fresh_dir(work / "out-cold")
        _, _, cold_code = timed(["node", str(tsc_js), "-p", str(work / "tsconfig.cold.json")], cwd=work)
        cold_digest = tree_digest(work / "out-cold")
        for name, worker in workers.items():
            fresh_dir(work / f"out-{name}")
            code, _, _, reused = parse_ts_response(
                worker.request(json.dumps({"project": str(work / f"tsconfig.{name}.json")}))[1]
            )
            digest = tree_digest(work / f"out-{name}")
            outcome[name] = {
                "exit": code,
                "reused_source_files": reused,
                "matches_cold": code == cold_code and digest == cold_digest,
            }
    finally:
        for worker in workers.values():
            worker.close()
    return {
        "id": "tsc-same-stat-edit",
        "toolchain": "tsc",
        "edit": "leaf module content changed, size and mtime_ns restored",
        "cold_exit": cold_code,
        "workers": outcome,
    }


def failure_mode(result: dict[str, object], cold_exit: int) -> str:
    """Name what a worker result would do to a build.

    `silent_wrong_output` is the dangerous one: exit 0 with bytes a cold
    compile would not produce, which a cache would then serve forever.
    `spurious_failure` is loud -- the build fails where a cold compile passes.
    """
    if result["matches_cold"]:
        return "correct"
    if result["exit"] == 0 and cold_exit == 0:
        return "silent_wrong_output"
    if result["exit"] != 0 and cold_exit == 0:
        return "spurious_failure"
    return "spurious_success"


def classify_hazards(hazards: list[dict[str, object]]) -> list[dict[str, object]]:
    """Classify each worker result and whether a cold rerun would catch it.

    The cold rerun is `--check-determinism` with its second execution forced
    out of the worker. It compares exit status and output digests, so it
    catches a silent wrong output or a spurious success. A spurious failure
    never reaches the rerun -- the first execution already failed the build --
    so it is recorded as visible rather than detected.
    """
    for hazard in hazards:
        for result in hazard["workers"].values():
            mode = failure_mode(result, hazard["cold_exit"])
            result["failure_mode"] = mode
            result["detected_by_cold_rerun"] = mode in ("silent_wrong_output", "spurious_success")
    return hazards


# --------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------


def source_state() -> dict[str, object]:
    commit = run(["git", "rev-parse", "HEAD"], cwd=REPO).stdout.strip()
    dirty = bool(run(["git", "status", "--porcelain"], cwd=REPO).stdout.strip())
    return {"commit": commit, "working_tree_dirty": dirty}


def cpu_model() -> str | None:
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        return None
    return None


def governor() -> str | None:
    path = pathlib.Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    try:
        return path.read_text(encoding="utf-8").strip()
    except OSError:
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--typescript-js", required=True, type=pathlib.Path, help="typescript@6 package directory")
    parser.add_argument("--tsc-native", type=pathlib.Path, help="native typescript@7 tsc executable (reference only)")
    parser.add_argument("--units", type=int, default=60)
    parser.add_argument("--iterations", type=int, default=21)
    parser.add_argument("--warmup", type=int, default=6)
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args()
    if args.iterations < args.warmup + 7:
        parser.error("--iterations must leave at least 7 steady-state samples after --warmup")

    typescript_js = args.typescript_js.resolve()
    tsc_native = args.tsc_native.resolve() if args.tsc_native else None
    load_before = list(os.getloadavg()) if hasattr(os, "getloadavg") else None
    started = time.perf_counter()
    with tempfile.TemporaryDirectory(prefix="frost-worker-bench-") as name:
        temporary = pathlib.Path(name)
        javac = bench_javac(temporary, units=args.units, iterations=args.iterations, warmup=args.warmup)
        tsc = bench_tsc(
            temporary,
            typescript_js=typescript_js,
            tsc_native=tsc_native,
            units=args.units,
            iterations=args.iterations,
            warmup=args.warmup,
        )
        classes = temporary / "worker-classes"
        hazards = classify_hazards(
            [
                *javac_classpath_hazard(temporary, classes),
                tsc_stat_cache_hazard(temporary, typescript_js, args.units),
            ]
        )
    typescript_version = json.loads((typescript_js / "package.json").read_text(encoding="utf-8"))["version"]
    report = {
        "schema": SCHEMA,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "cpu_model": cpu_model(),
            "cpu_count": os.cpu_count(),
            "governor": governor(),
            "load_avg_before": load_before,
            "load_avg_after": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
            "note": "shared host with other concurrent workloads; compare ratios within this run, not absolute times across runs",
        },
        "tools": {
            "java": first_line(["java", "-version"]),
            "javac": first_line(["javac", "-version"]),
            "node": first_line(["node", "--version"]),
            "typescript_js": typescript_version,
            "typescript_native": first_line([str(tsc_native), "--version"]) if tsc_native else None,
            "python": platform.python_version(),
        },
        "source": source_state(),
        "reproduce": (
            "python3 scripts/bench_persistent_worker.py "
            "--typescript-js <prefix>/node_modules/typescript "
            "--tsc-native <prefix>/node_modules/@typescript/typescript-linux-x64/lib/tsc "
            f"--units {args.units} --iterations {args.iterations} --warmup {args.warmup} --out {args.out}"
        ),
        "fixture": {
            "generator": "scripts/bench_persistent_worker.py (java_sources, typescript_sources)",
            "units": args.units,
            "edit": "every iteration flips one leaf constant between two same-length spellings; all units depend on it",
            "order": "scenario order rotates every iteration",
            "iterations": args.iterations,
            "warmup_requests": args.warmup,
        },
        "javac": javac,
        "tsc": tsc,
        "hazards": hazards,
        "elapsed_s": round(time.perf_counter() - started, 1),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
