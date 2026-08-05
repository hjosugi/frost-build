"""The page's script parses.

`site/script.js` is loaded by a static page with no build step, no bundler and
no type checker, so a stray brace ships. Nothing else in the repository looks at
it: the CSS has `tests.test_site_css`, the mark has `tests.test_site_mark`, and
the VS Code extension has its own `node --test` job because its code is
imported by tests. This file is none of those — it runs in a browser, and the
first reader of a syntax error would be a visitor with a blank hero.

`node --check` is a parse, not a lint: it says the file is syntactically valid
JavaScript and nothing about whether it works. That is the cheap half of the
problem and the half that fails silently.
"""

import pathlib
import shutil
import subprocess
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "site" / "script.js"


class SiteScriptTest(unittest.TestCase):
    def test_the_page_script_parses(self):
        # The GitHub runner image ships Node, so this skips on a developer
        # machine without one and runs on every push, which is where a broken
        # page would otherwise be published from.
        node = shutil.which("node")
        if node is None:
            self.skipTest("node is not installed")
        result = subprocess.run(
            [node, "--check", str(SCRIPT)],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
