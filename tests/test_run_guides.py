"""The guide runner executes what a page says, and fails when a page is wrong.

The tutorials are only as trustworthy as the script that runs them: a runner
that skipped a block, ignored an exit status or compared output loosely would
report a stale page as green. These tests pin each of those behaviours on
throwaway pages, with a stand-in ``frost`` so they need no Rust build.
"""

from __future__ import annotations

import importlib.util
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("run_guides", ROOT / "scripts" / "run_guides.py")
assert SPEC and SPEC.loader
RUN_GUIDES = importlib.util.module_from_spec(SPEC)
# dataclasses resolve annotations through sys.modules.
sys.modules["run_guides"] = RUN_GUIDES
SPEC.loader.exec_module(RUN_GUIDES)

PAGE = """# A page

<!-- guide-test: requires=sh dir=demo -->

```c file=src/a.c
int a;
```

```toml
# prose only, never executed
```

```sh
frost greet
```

```text output
hello from the stand-in
```

```sh fails
exit 3
```

```sh skip
this would fail if it ran
```
"""


def stand_in_frost(directory: Path) -> Path:
    frost = directory / "frost"
    frost.write_text("#!/bin/sh\necho 'hello from the stand-in'\npwd\n", encoding="utf-8")
    frost.chmod(frost.stat().st_mode | stat.S_IXUSR)
    return frost


@unittest.skipIf(os.name == "nt", "the runner executes bash blocks")
class GuideRunnerTests(unittest.TestCase):
    def run_text(self, text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            page = RUN_GUIDES.parse_page(root / "page.md", text)
            workdir = root / page.directory
            workdir.mkdir()
            return RUN_GUIDES.run_page(page, stand_in_frost(root), workdir, verbose=False)

    def test_a_page_is_parsed_into_its_executable_steps(self):
        page = RUN_GUIDES.parse_page(Path("page.md"), PAGE)
        self.assertEqual(["sh"], page.requires)
        self.assertEqual("demo", page.directory)
        self.assertEqual(["file", "run", "output", "run"], [step.kind for step in page.steps])
        self.assertEqual("src/a.c", page.steps[0].path)
        self.assertEqual("int a;\n", page.steps[0].body)
        self.assertTrue(page.steps[3].expect_failure)

    def test_an_unmarked_page_is_not_executable(self):
        self.assertIsNone(RUN_GUIDES.parse_page(Path("page.md"), "# Prose\n\n```sh\nls\n```\n"))

    def test_a_correct_page_passes_and_files_land_in_the_page_directory(self):
        self.assertEqual([], self.run_text(PAGE))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            page = RUN_GUIDES.parse_page(root / "page.md", PAGE + "\n```sh\ntest -f src/a.c\n```\n")
            workdir = root / "demo"
            workdir.mkdir()
            self.assertEqual([], RUN_GUIDES.run_page(page, stand_in_frost(root), workdir, False))

    def test_missing_output_fails_and_shows_what_was_printed(self):
        problems = self.run_text(PAGE.replace("hello from the stand-in\n```", "goodbye\n```"))
        self.assertEqual(1, len(problems))
        self.assertIn("did not print", problems[0])
        self.assertIn("goodbye", problems[0])
        self.assertIn("hello from the stand-in", problems[0])

    def test_a_failing_command_fails_the_page(self):
        problems = self.run_text(PAGE.replace("frost greet", "frost greet\nfalse"))
        self.assertEqual(1, len(problems))
        self.assertIn("expected the block to succeed", problems[0])

    def test_a_command_expected_to_fail_must_fail(self):
        problems = self.run_text(PAGE.replace("exit 3", "true"))
        self.assertEqual(1, len(problems))
        self.assertIn("expected the block to fail", problems[0])

    def test_malformed_pages_are_refused_before_anything_runs(self):
        for text in (
            "<!-- guide-test: colour=blue -->\n```sh\nls\n```\n",
            "<!-- guide-test: -->\n```c file=../escape.c\nx\n```\n```sh\nls\n```\n",
            "<!-- guide-test: -->\n```sh\nls\n",
            "<!-- guide-test: -->\nno commands at all\n",
            "<!-- guide-test: -->\n```sh sometimes\nls\n```\n",
        ):
            with self.subTest(text=text):
                with self.assertRaises(RUN_GUIDES.GuideError):
                    RUN_GUIDES.parse_page(Path("page.md"), text)

    def test_every_guide_page_in_the_repository_parses(self):
        pages = [
            RUN_GUIDES.parse_page(path, path.read_text(encoding="utf-8"))
            for path in sorted((ROOT / "docs" / "guide").rglob("*.md"))
        ]
        executable = [page for page in pages if page is not None]
        # Three tutorials plus the migration guides that can run on a CI host.
        self.assertGreaterEqual(len(executable), 6)


if __name__ == "__main__":
    unittest.main()
