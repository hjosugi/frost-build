"""Tests for the scale/soak workspace generator and its analysis helpers.

The generator is only evidence if the same arguments always produce the same
workspace, and if each named shape really has the property it is named for, so
both are asserted here without needing a frost binary.
"""

import dataclasses
import pathlib
import random
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "scripts"))

import frost_scale  # noqa: E402


def small(name: str, **overrides) -> frost_scale.Shape:
    return dataclasses.replace(frost_scale.PRESETS[name], **overrides)


class GeneratorTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def generate(self, shape: frost_scale.Shape, name: str = "ws") -> frost_scale.WorkspaceModel:
        return frost_scale.generate(self.root / name, shape)

    def manifests(self, name: str = "ws") -> dict[str, dict]:
        base = self.root / name
        return {
            path.parent.relative_to(base).as_posix(): tomllib.loads(path.read_text())
            for path in sorted(base.rglob("frost.toml"))
        }

    def test_generation_is_deterministic(self) -> None:
        shape = small("soak", targets=120, packages=6, output_dir_files=5)
        first = self.generate(shape, "a")
        second = self.generate(shape, "b")
        self.assertEqual(
            frost_scale.tree_digest(self.root / "a"), frost_scale.tree_digest(self.root / "b")
        )
        self.assertEqual(first.actions, second.actions)
        self.assertEqual(sorted(first.targets), sorted(second.targets))

        reseeded = self.generate(dataclasses.replace(shape, seed=shape.seed + 1), "c")
        self.assertNotEqual(
            frost_scale.tree_digest(self.root / "a"), frost_scale.tree_digest(self.root / "c")
        )
        self.assertEqual(len(reseeded.targets), len(first.targets))

    def test_refuses_to_write_into_a_populated_directory(self) -> None:
        (self.root / "ws").mkdir()
        (self.root / "ws" / "keep.txt").write_text("mine")
        with self.assertRaises(ValueError):
            self.generate(small("wide", targets=4))

    def test_linear_shape_is_one_chain(self) -> None:
        model = self.generate(small("linear", targets=30))
        leaves = [t for t in model.targets.values() if t.label.split(":")[1].startswith("t")]
        self.assertEqual(len(leaves), 30)
        self.assertEqual(sum(1 for t in leaves if not t.deps), 1)
        self.assertTrue(all(len(t.deps) <= 1 for t in leaves))
        # Editing the first link reruns every link, the package root and `all`.
        self.assertEqual(model.affected_actions([leaves[0].sources[0]]), 32)

    def test_wide_shape_bounds_every_fan_in(self) -> None:
        shape = small("wide", targets=500, fanout=16)
        model = self.generate(shape)
        widths = [len(t.deps) for t in model.targets.values()]
        self.assertLessEqual(max(widths), 16)
        leaves = [t for t in model.targets.values() if not t.deps and t.sources]
        self.assertEqual(len(leaves), 500)
        # A leaf reaches the root through a logarithmic number of levels.
        self.assertLessEqual(model.affected_actions([leaves[0].sources[0]]), 6)

    def test_packages_shape_writes_one_manifest_per_package(self) -> None:
        shape = small("packages", targets=90, packages=9)
        model = self.generate(shape)
        manifests = self.manifests()
        self.assertEqual(len(manifests), 10)
        self.assertIn("workspace", manifests["."])
        cross = [
            t
            for t in model.targets.values()
            if t.label.startswith("//pkgs/")
            and any(d.split(":")[0] != t.label.split(":")[0] for d in t.deps)
        ]
        self.assertTrue(cross, "no package root reads another package")

    def test_headers_shape_generates_headers_compiled_by_c(self) -> None:
        shape = small("headers", targets=20, packages=2, generated_headers=5, cc_sources=3)
        model = self.generate(shape)
        headers = [t for t in model.targets.values() if t.label.endswith(":headers")]
        self.assertEqual(len(headers), 2)
        self.assertTrue(all(len(t.outputs) == 5 for t in headers))
        libs = [t for t in model.targets.values() if t.kind == "cc_library"]
        self.assertTrue(all(t.actions == 4 for t in libs))
        source = (self.root / "ws" / libs[0].sources[0]).read_text()
        self.assertIn('#include "p0000_h', source)
        # A header input reruns its genrule, every compile, the archive and
        # the package root chain above them.
        self.assertGreaterEqual(model.affected_actions([model.header_inputs[0]]), 1 + 4 + 1)

    def test_output_dirs_shape_declares_owned_trees(self) -> None:
        shape = small("output-dirs", targets=10, output_dir_targets=3, output_dir_files=7)
        model = self.generate(shape)
        root = self.manifests()["."]
        trees = {name: t for name, t in root["target"].items() if "output_dirs" in t}
        self.assertEqual(len(trees), 3)
        for target in trees.values():
            self.assertIn("${config}", target["output_dirs"][0])
            self.assertIn("7", target["args"])
        self.assertEqual(len(model.declared_output_dirs()), 3)

    def test_monorepo_preset_is_the_issue_target_shape(self) -> None:
        shape = frost_scale.PRESETS["monorepo"]
        self.assertEqual(shape.targets, 50_000)
        self.assertEqual(shape.targets * shape.files_per_target, 100_000)
        self.assertGreater(shape.packages, 100)
        self.assertTrue(shape.generated_headers and shape.cc_sources)
        self.assertTrue(shape.output_dir_targets and shape.output_dir_files)

    def test_every_generated_manifest_parses_and_labels_resolve(self) -> None:
        shape = small("soak", targets=64, packages=4, output_dir_files=2)
        model = self.generate(shape)
        manifests = self.manifests()
        declared = set()
        for package, manifest in manifests.items():
            prefix = "//" + ("" if package == "." else package)
            for name in manifest.get("target", {}):
                declared.add(f"{prefix}:{name}")
        self.assertEqual(declared, set(model.targets))
        for target in model.targets.values():
            for dep in target.deps:
                self.assertIn(dep, model.targets)

    def test_copy_sources_leaves_build_products_behind(self) -> None:
        shape = small("output-dirs", targets=4, output_dir_targets=1, output_dir_files=2)
        model = self.generate(shape)
        ws = self.root / "ws"
        for rel in model.declared_outputs():
            (ws / rel).parent.mkdir(parents=True, exist_ok=True)
            (ws / rel).write_text("built")
        for rel in model.declared_output_dirs():
            (ws / rel).mkdir(parents=True)
            (ws / rel / "f0.txt").write_text("built")
        (ws / ".frost").mkdir()
        (ws / ".frost" / "journal.bin").write_text("state")
        frost_scale.copy_sources(ws, self.root / "copy", model)
        self.assertFalse((self.root / "copy" / ".frost").exists())
        for rel in model.declared_outputs() + model.declared_output_dirs():
            self.assertFalse((self.root / "copy" / rel).exists(), rel)
        self.assertTrue((self.root / "copy" / "frost.toml").is_file())


class CksumTest(unittest.TestCase):
    def test_known_vectors(self) -> None:
        self.assertEqual(frost_scale.posix_cksum(b""), "4294967295 0\n")
        self.assertEqual(frost_scale.posix_cksum(b"123456789"), "930766865 9\n")

    @unittest.skipUnless(shutil.which("cksum"), "no cksum on PATH")
    def test_matches_the_host_cksum(self) -> None:
        rng = random.Random(149)
        for size in (1, 7, 255, 256, 4096, 70000):
            data = bytes(rng.randrange(256) for _ in range(size))
            host = subprocess.run(["cksum"], input=data, capture_output=True, check=True).stdout
            self.assertEqual(frost_scale.posix_cksum(data), host.decode())


class LeakAnalysisTest(unittest.TestCase):
    def series(self, fds, rss):
        return [{"fds": f, "rss_kib": r, "threads": 4} for f, r in zip(fds, rss)]

    def test_flat_series_passes(self) -> None:
        samples = self.series([12] * 50, [40_000 + (i % 3) * 100 for i in range(50)])
        result = frost_scale.analyse_leaks(samples, fd_slack=4, rss_growth=1.5, rss_slack_kib=0)
        self.assertTrue(result["ok"], result)

    def test_growing_descriptors_fail(self) -> None:
        samples = self.series(list(range(10, 60)), [40_000] * 50)
        result = frost_scale.analyse_leaks(samples, fd_slack=4, rss_growth=1.5, rss_slack_kib=0)
        self.assertFalse(result["ok"])
        self.assertGreater(result["fd_end"], result["fd_baseline"] + 4)

    def test_growing_memory_fails(self) -> None:
        samples = self.series([12] * 50, [40_000 * (1 + i // 5) for i in range(50)])
        result = frost_scale.analyse_leaks(samples, fd_slack=4, rss_growth=1.5, rss_slack_kib=1024)
        self.assertFalse(result["ok"])

    def test_warm_up_growth_is_not_a_leak(self) -> None:
        rss = [10_000 * (i + 1) for i in range(5)] + [60_000] * 45
        result = frost_scale.analyse_leaks(
            self.series([12] * 50, rss), fd_slack=4, rss_growth=1.25, rss_slack_kib=0
        )
        self.assertTrue(result["ok"], result)

    def test_too_few_samples_is_not_a_pass(self) -> None:
        result = frost_scale.analyse_leaks(
            self.series([1] * 3, [1] * 3), fd_slack=4, rss_growth=1.5, rss_slack_kib=0
        )
        self.assertFalse(result["ok"])


class EventCountTest(unittest.TestCase):
    def test_counts_executed_and_cached_results(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "events.ndjson"
            path.write_text(
                "\n".join(
                    [
                        '{"event":"build_started","actions":3}',
                        '{"event":"action_finished","result":"executed"}',
                        '{"event":"action_finished","result":"flaky"}',
                        '{"event":"action_finished","result":"cached"}',
                        '{"event":"build_finished","success":true}',
                    ]
                )
            )
            counts = frost_scale.count_events(path)
        self.assertEqual(counts["executed"], 2)
        self.assertEqual(counts["cached"], 1)
        self.assertEqual(counts["total"], 3)


if __name__ == "__main__":
    unittest.main()
