from __future__ import annotations

import hashlib
import http.server
import importlib.util
import json
import os
import subprocess
import tarfile
import tempfile
import threading
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "install.sh"
RENDERER_PATH = ROOT / "scripts" / "render_distribution_manifests.py"
SPEC = importlib.util.spec_from_file_location("render_distribution_manifests", RENDERER_PATH)
assert SPEC and SPEC.loader
RENDERER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RENDERER)


class FixtureServer:
    def __init__(self, files: dict[str, bytes]):
        self.files = files
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                body = fixture.files.get(self.path)
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def release_archive(root: Path, version: str) -> tuple[str, bytes]:
    triple = "x86_64-unknown-linux-musl"
    directory = f"frostbuild-v{version}-{triple}"
    tree = root / directory
    (tree / "share/man/man1").mkdir(parents=True)
    (tree / "share/completions").mkdir(parents=True)
    for binary, output in (("frost", f"frost {version}\n"), ("frostd", "")):
        path = tree / binary
        path.write_text(f"#!/bin/sh\nprintf '%s' '{output}'\n", encoding="utf-8")
        path.chmod(0o755)
    (tree / "share/man/man1/frost.1").write_text(".TH frost 1\n", encoding="utf-8")
    for name in ("frost.bash", "_frost", "frost.fish"):
        (tree / "share/completions" / name).write_text(f"# {name}\n", encoding="utf-8")
    archive_name = f"{directory}.tar.gz"
    archive_path = root / archive_name
    with tarfile.open(archive_path, "w:gz") as archive:
        archive.add(tree, arcname=directory)
    return archive_name, archive_path.read_bytes()


class DistributionTest(unittest.TestCase):
    def test_install_script_verifies_then_publishes_the_complete_prefix(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            archive_name, archive = release_archive(root, version)
            digest = hashlib.sha256(archive).hexdigest()
            files = {
                "/latest": json.dumps({"tag_name": f"v{version}"}).encode(),
                f"/v{version}/SHA256SUMS": f"{digest}  {archive_name}\n".encode(),
                f"/v{version}/{archive_name}": archive,
            }
            server = FixtureServer(files)
            try:
                prefix = root / "prefix"
                environment = os.environ | {
                    "FROST_INSTALL_API_URL": f"{server.base_url}/latest",
                    "FROST_INSTALL_RELEASE_BASE_URL": server.base_url,
                    "FROST_INSTALL_OS": "Linux",
                    "FROST_INSTALL_ARCH": "x86_64",
                    "NO_PROXY": "*",
                    "no_proxy": "*",
                }
                result = subprocess.run(
                    ["sh", str(INSTALL), "--prefix", str(prefix)],
                    text=True,
                    capture_output=True,
                    env=environment,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                reported = subprocess.check_output([prefix / "bin/frost", "--version"], text=True)
                self.assertEqual(reported, f"frost {version}\n")
                self.assertTrue((prefix / "bin/frostd").is_file())
                self.assertTrue((prefix / "share/man/man1/frost.1").is_file())
                self.assertTrue((prefix / "share/bash-completion/completions/frost").is_file())
            finally:
                server.close()

    def test_install_script_rejects_tampering_before_creating_the_prefix(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            archive_name, archive = release_archive(root, version)
            digest = hashlib.sha256(archive).hexdigest()
            tampered = bytearray(archive)
            tampered[len(tampered) // 2] ^= 0xFF
            server = FixtureServer(
                {
                    f"/v{version}/SHA256SUMS": f"{digest}  {archive_name}\n".encode(),
                    f"/v{version}/{archive_name}": bytes(tampered),
                }
            )
            try:
                prefix = root / "must-not-exist"
                result = subprocess.run(
                    ["sh", str(INSTALL), "--version", version, "--prefix", str(prefix)],
                    text=True,
                    capture_output=True,
                    env=os.environ
                    | {
                        "FROST_INSTALL_RELEASE_BASE_URL": server.base_url,
                        "FROST_INSTALL_OS": "Linux",
                        "FROST_INSTALL_ARCH": "x86_64",
                        "NO_PROXY": "*",
                        "no_proxy": "*",
                    },
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("checksum mismatch", result.stderr)
                self.assertFalse(prefix.exists())
            finally:
                server.close()

    def test_package_manifests_are_derived_only_from_release_checksums(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "1.2.3"
            names = [
                f"frostbuild-v{version}-x86_64-unknown-linux-musl.tar.gz",
                f"frostbuild-v{version}-aarch64-apple-darwin.tar.gz",
                f"frostbuild-v{version}-x86_64-pc-windows-msvc.zip",
            ]
            sums = root / "SHA256SUMS"
            sums.write_text(
                "".join(f"{str(index) * 64}  {name}\n" for index, name in enumerate(names, 1)),
                encoding="utf-8",
            )
            output = root / "out"
            RENDERER.render(version, sums, output)
            formula = (output / "frostbuild.rb").read_text(encoding="utf-8")
            manifest = json.loads((output / "frostbuild.json").read_text(encoding="utf-8"))
            self.assertIn(f'version "{version}"', formula)
            self.assertIn(names[0], formula)
            self.assertIn(names[1], formula)
            self.assertEqual(manifest["architecture"]["64bit"]["hash"], "3" * 64)
            self.assertEqual(manifest["bin"], ["frost.exe", "frostd.exe"])

    def test_manifest_renderer_rejects_mixed_or_incomplete_releases(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sums = root / "SHA256SUMS"
            sums.write_text(
                f"{'a' * 64}  frostbuild-v1.2.2-x86_64-unknown-linux-musl.tar.gz\n",
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                RENDERER.render("1.2.3", sums, root / "out")

            sums.write_text(
                f"{'a' * 64}  frostbuild-v1.2.3-x86_64-unknown-linux-musl.tar.gz\n",
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                RENDERER.render("1.2.3", sums, root / "out")
            self.assertFalse((root / "out").exists())

            duplicate = f"frostbuild-v1.2.3-x86_64-unknown-linux-musl.tar.gz"
            sums.write_text(
                f"{'a' * 64}  {duplicate}\n{'b' * 64}  {duplicate}\n",
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                RENDERER.render("1.2.3", sums, root / "out")


if __name__ == "__main__":
    unittest.main()
