from __future__ import annotations

import hashlib
import http.server
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import threading
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "install.sh"
VERIFY_RELEASE = ROOT / "scripts" / "verify_release.sh"


def load_script(name: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RENDERER = load_script("render_distribution_manifests")
PACKAGER = load_script("package_release")

RELEASE_IDENTITY = (
    r"^https://github\.com/hjosugi/frost-build/\.github/workflows/release\.yml"
    r"@refs/(heads/main|tags/v[0-9]+\.[0-9]+\.[0-9]+)$"
)
SIGSTORE_ISSUER = "https://token.actions.githubusercontent.com"

# Stand-ins for cosign and gh. They cannot do Sigstore cryptography, so the
# fixture's "bundle" is the SHA-256 of the one blob it vouches for and its
# "provenance" is the list of subject digests. What they do test is the part
# that is this repository's: which file is handed to which tool with which
# identity, and that a refusal from the tool stops the installation. The real
# tools run against real signatures in .github/workflows/release-dry-run.yml.
FAKE_TOOL = textwrap.dedent(
    """\
    #!{python}
    import hashlib, json, os, sys
    arguments = sys.argv[1:]
    with open(os.environ["FAKE_TOOL_LOG"], "a", encoding="utf-8") as log:
        log.write(json.dumps([{name!r}] + arguments) + "\\n")
    def digest(path):
        with open(path, "rb") as handle:
            return hashlib.sha256(handle.read()).hexdigest()
    bundle = arguments[arguments.index("--bundle") + 1]
    with open(bundle, encoding="utf-8") as handle:
        vouched = handle.read().split()
    subject = arguments[-1] if {name!r} == "cosign" else arguments[2]
    sys.exit(0 if digest(subject) in vouched else 1)
    """
)


def fake_tools(directory: Path, *names: str) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    for name in names:
        tool = directory / name
        tool.write_text(FAKE_TOOL.format(python=sys.executable, name=name), encoding="utf-8")
        tool.chmod(0o755)
    return directory


def logged_calls(log: Path) -> list[list[str]]:
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


def argument(call: list[str], flag: str) -> str:
    return call[call.index(flag) + 1]


class FixtureServer:
    def __init__(self, files: dict[str, bytes]):
        self.files = files
        self.requests: list[str] = []
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append(self.path)
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


def signed_release(root: Path, version: str, archive_name: str, archive: bytes) -> dict[str, bytes]:
    """A fixture release whose stand-in signature and provenance vouch for it."""
    digest = hashlib.sha256(archive).hexdigest()
    sums = f"{digest}  {archive_name}\n".encode()
    return {
        f"/v{version}/SHA256SUMS": sums,
        f"/v{version}/SHA256SUMS.sigstore.json": hashlib.sha256(sums).hexdigest().encode(),
        f"/v{version}/{archive_name}": archive,
        f"/v{version}/frostbuild-v{version}.intoto.jsonl": f"{digest}\n".encode(),
    }


def install_environment(server: FixtureServer, **extra: str) -> dict[str, str]:
    return (
        os.environ
        | {
            "FROST_INSTALL_RELEASE_BASE_URL": server.base_url,
            "FROST_INSTALL_OS": "Linux",
            "FROST_INSTALL_ARCH": "x86_64",
            "NO_PROXY": "*",
            "no_proxy": "*",
        }
        | extra
    )


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

    def test_install_script_checks_signature_and_provenance_only_when_asked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            archive_name, archive = release_archive(root, version)
            server = FixtureServer(signed_release(root, version, archive_name, archive))
            log = root / "tools.log"
            tools = fake_tools(root / "tools", "cosign", "gh")
            path = f"{tools}{os.pathsep}{os.environ['PATH']}"
            try:
                prefix = root / "prefix"
                result = subprocess.run(
                    ["sh", str(INSTALL), "--version", version, "--prefix", str(prefix), "--verify-signature"],
                    text=True,
                    capture_output=True,
                    env=install_environment(
                        server,
                        PATH=path,
                        FAKE_TOOL_LOG=str(log),
                        FROST_INSTALL_VERIFY_PROVENANCE="1",
                    ),
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                calls = logged_calls(log)
                self.assertEqual([call[0] for call in calls], ["cosign", "gh"])
                cosign, gh = calls
                self.assertEqual(cosign[1], "verify-blob")
                self.assertEqual(argument(cosign, "--certificate-identity-regexp"), RELEASE_IDENTITY)
                self.assertEqual(argument(cosign, "--certificate-oidc-issuer"), SIGSTORE_ISSUER)
                self.assertEqual(Path(cosign[-1]).name, "SHA256SUMS")
                self.assertEqual(gh[1:3], ["attestation", "verify"])
                self.assertEqual(Path(gh[3]).name, archive_name)
                self.assertEqual(argument(gh, "--repo"), "hjosugi/frost-build")
                self.assertEqual(
                    argument(gh, "--signer-workflow"),
                    "hjosugi/frost-build/.github/workflows/release.yml",
                )
                self.assertEqual(Path(argument(gh, "--bundle")).name, f"frostbuild-v{version}.intoto.jsonl")
                self.assertIn("SHA256SUMS signature verified", result.stderr)
                self.assertIn("build provenance verified", result.stderr)
                reported = subprocess.check_output([prefix / "bin/frost", "--version"], text=True)
                self.assertEqual(reported, f"frost {version}\n")

                # The default stays the checksum alone: neither tool runs and
                # neither verification file is fetched.
                log.unlink()
                server.requests.clear()
                result = subprocess.run(
                    ["sh", str(INSTALL), "--version", version, "--prefix", str(root / "plain")],
                    text=True,
                    capture_output=True,
                    env=install_environment(server, PATH=path, FAKE_TOOL_LOG=str(log)),
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(logged_calls(log), [])
                self.assertEqual(
                    sorted(server.requests),
                    sorted([f"/v{version}/SHA256SUMS", f"/v{version}/{archive_name}"]),
                )
            finally:
                server.close()

    def test_signature_check_rejects_a_consistently_rewritten_release(self):
        # Whoever can replace a release's archive can replace its SHA256SUMS
        # too, and then the checksum passes. The signature over the genuine
        # list is what they cannot reproduce.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            archive_name, archive = release_archive(root, version)
            files = signed_release(root, version, archive_name, archive)
            tampered = bytearray(archive)
            tampered[len(tampered) // 2] ^= 0xFF
            files[f"/v{version}/{archive_name}"] = bytes(tampered)
            files[f"/v{version}/SHA256SUMS"] = (
                f"{hashlib.sha256(tampered).hexdigest()}  {archive_name}\n".encode()
            )
            server = FixtureServer(files)
            tools = fake_tools(root / "tools", "cosign", "gh")
            try:
                prefix = root / "must-not-exist"
                result = subprocess.run(
                    ["sh", str(INSTALL), "--version", version, "--prefix", str(prefix), "--verify-signature"],
                    text=True,
                    capture_output=True,
                    env=install_environment(
                        server,
                        PATH=f"{tools}{os.pathsep}{os.environ['PATH']}",
                        FAKE_TOOL_LOG=str(root / "tools.log"),
                    ),
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("SHA256SUMS is not signed by hjosugi/frost-build", result.stderr)
                self.assertFalse(prefix.exists())
                # Refused before the archive was even downloaded.
                self.assertNotIn(f"/v{version}/{archive_name}", server.requests)
            finally:
                server.close()

    def test_provenance_check_rejects_an_unattested_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            archive_name, archive = release_archive(root, version)
            files = signed_release(root, version, archive_name, archive)
            files[f"/v{version}/frostbuild-v{version}.intoto.jsonl"] = b"0" * 64 + b"\n"
            server = FixtureServer(files)
            tools = fake_tools(root / "tools", "cosign", "gh")
            try:
                prefix = root / "must-not-exist"
                result = subprocess.run(
                    ["sh", str(INSTALL), "--version", version, "--prefix", str(prefix), "--verify-provenance"],
                    text=True,
                    capture_output=True,
                    env=install_environment(
                        server,
                        PATH=f"{tools}{os.pathsep}{os.environ['PATH']}",
                        FAKE_TOOL_LOG=str(root / "tools.log"),
                    ),
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{archive_name} has no valid provenance", result.stderr)
                self.assertFalse(prefix.exists())
            finally:
                server.close()

    def test_a_requested_check_without_its_tool_refuses_before_any_download(self):
        shell = shutil.which("sh")
        assert shell
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            server = FixtureServer({})
            nothing = root / "empty-path"
            nothing.mkdir()
            only_cosign = fake_tools(root / "cosign-only", "cosign")
            try:
                for flag, path, message in (
                    ("--verify-signature", nothing, "--verify-signature needs cosign on PATH"),
                    ("--verify-provenance", only_cosign, "--verify-provenance needs the GitHub CLI"),
                ):
                    prefix = root / "never"
                    result = subprocess.run(
                        [shell, str(INSTALL), "--version", "9.8.7", "--prefix", str(prefix), flag],
                        text=True,
                        capture_output=True,
                        env=install_environment(server, PATH=str(path)),
                        check=False,
                    )
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(prefix.exists())

                # A mistyped switch is refused rather than read as "off".
                result = subprocess.run(
                    [shell, str(INSTALL), "--version", "9.8.7", "--prefix", str(root / "never")],
                    text=True,
                    capture_output=True,
                    env=install_environment(server, FROST_INSTALL_VERIFY_SIGNATURE="yes"),
                    check=False,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("FROST_INSTALL_VERIFY_SIGNATURE must be 0 or 1", result.stderr)
                self.assertEqual(server.requests, [])
            finally:
                server.close()

    def test_release_archives_are_byte_reproducible_and_installable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            binaries = root / "binaries"
            binaries.mkdir()
            for binary, output in (("frost", f"frost {version}\n"), ("frostd", "")):
                for suffix in ("", ".exe"):
                    path = binaries / f"{binary}{suffix}"
                    path.write_text(f"#!/bin/sh\nprintf '%s' '{output}'\n", encoding="utf-8")
                    path.chmod(0o755)
            assets = root / "assets"
            (assets / "share/man/man1").mkdir(parents=True)
            (assets / "share/completions").mkdir(parents=True)
            (assets / "share/man/man1/frost.1").write_text(".TH frost 1\n", encoding="utf-8")
            (assets / "share/man/man1/frost-build.1").write_text(".TH frost-build 1\n", encoding="utf-8")
            for name in ("frost.bash", "_frost", "frost.fish"):
                (assets / "share/completions" / name).write_text(f"# {name}\n", encoding="utf-8")

            epoch = 1_600_000_000
            builds = []
            # Two packings of the same inputs whose files carry different
            # times, as two builds of one tag on two runners would.
            for attempt, stamp in enumerate((1_000_000_000, 1_700_000_000)):
                for path in [*binaries.iterdir(), *assets.rglob("*")]:
                    os.utime(path, (stamp, stamp))
                builds.append(
                    [
                        PACKAGER.package(
                            f"v{version}", triple, binaries, assets, root / f"out{attempt}", fmt, ROOT, epoch
                        ).read_bytes()
                        for triple, fmt in (
                            ("x86_64-unknown-linux-musl", "tar.gz"),
                            ("x86_64-pc-windows-msvc", "zip"),
                        )
                    ]
                )
            self.assertEqual(builds[0], builds[1])

            top = f"frostbuild-v{version}-x86_64-unknown-linux-musl"
            tar_path = root / "out0" / f"{top}.tar.gz"
            self.assertEqual(tar_path.read_bytes()[4:8], b"\0\0\0\0", "gzip header carries a timestamp")
            with tarfile.open(tar_path) as archive:
                members = archive.getmembers()
            names = [member.name for member in members]
            self.assertEqual(names, sorted(names))
            self.assertEqual(names[0], top)
            self.assertTrue(all(member.isfile() or member.isdir() for member in members))
            self.assertTrue(all((member.uid, member.gid, member.uname, member.gname) == (0, 0, "", "") for member in members))
            self.assertTrue(all(member.mtime == epoch for member in members))
            modes = {member.name.removeprefix(top + "/"): member.mode for member in members}
            self.assertEqual(modes["frost"], 0o755)
            self.assertEqual(modes["install.sh"], 0o755)
            self.assertEqual(modes["README.md"], 0o644)
            self.assertEqual(modes["share/man/man1/frost.1"], 0o644)

            windows = f"frostbuild-v{version}-x86_64-pc-windows-msvc"
            with zipfile.ZipFile(root / "out0" / f"{windows}.zip") as archive:
                entries = archive.infolist()
            self.assertEqual(entries[0].filename, f"{windows}/")
            self.assertIn(f"{windows}/frost.exe", [entry.filename for entry in entries])
            self.assertIn(f"{windows}/share/completions/_frost", [entry.filename for entry in entries])
            self.assertTrue(all(entry.date_time == (2020, 9, 13, 12, 26, 40) for entry in entries))

            # The deterministic layout is exactly what the installer accepts.
            archive_bytes = tar_path.read_bytes()
            digest = hashlib.sha256(archive_bytes).hexdigest()
            server = FixtureServer(
                {
                    f"/v{version}/SHA256SUMS": f"{digest}  {top}.tar.gz\n".encode(),
                    f"/v{version}/{top}.tar.gz": archive_bytes,
                }
            )
            try:
                prefix = root / "prefix"
                result = subprocess.run(
                    ["sh", str(INSTALL), "--version", version, "--prefix", str(prefix)],
                    text=True,
                    capture_output=True,
                    env=install_environment(server),
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue((prefix / "share/man/man1/frost-build.1").is_file())
            finally:
                server.close()

    def test_verify_release_checks_every_asset_and_names_the_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version = "9.8.7"
            release = root / "release"
            release.mkdir()
            names = [
                f"frostbuild-v{version}-x86_64-unknown-linux-musl.tar.gz",
                f"frostbuild-v{version}-x86_64-pc-windows-msvc.zip",
            ]
            sboms = [name.removesuffix(".tar.gz").removesuffix(".zip") + ".cdx.json" for name in names]
            vouched = []
            for index, (name, sbom_name) in enumerate(zip(names, sboms)):
                data = f"archive {index}\n".encode()
                (release / name).write_bytes(data)
                (release / f"{name}.sigstore.json").write_text(hashlib.sha256(data).hexdigest())
                sbom = json.dumps(
                    {
                        "bomFormat": "CycloneDX",
                        "specVersion": "1.5",
                        "metadata": {"component": {"name": "frostbuild-cli", "version": version}},
                        "components": [{"name": "blake3"}],
                    }
                ).encode()
                (release / sbom_name).write_bytes(sbom)
                vouched += [hashlib.sha256(data).hexdigest(), hashlib.sha256(sbom).hexdigest()]
            sums = "".join(
                f"{hashlib.sha256((release / name).read_bytes()).hexdigest()}  {name}\n" for name in names
            ).encode()
            (release / "SHA256SUMS").write_bytes(sums)
            (release / "SHA256SUMS.sigstore.json").write_text(hashlib.sha256(sums).hexdigest())
            vouched.append(hashlib.sha256(sums).hexdigest())
            (release / f"frostbuild-v{version}.intoto.jsonl").write_text("\n".join(vouched))

            log = root / "tools.log"
            tools = fake_tools(root / "tools", "cosign", "gh")
            environment = os.environ | {
                "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
                "FAKE_TOOL_LOG": str(log),
            }

            def verify(directory: Path) -> subprocess.CompletedProcess[str]:
                return subprocess.run(
                    ["bash", str(VERIFY_RELEASE), str(directory), version],
                    text=True,
                    capture_output=True,
                    env=environment,
                    check=False,
                )

            result = verify(release)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            calls = logged_calls(log)
            cosign = [call for call in calls if call[0] == "cosign"]
            gh = [call for call in calls if call[0] == "gh"]
            self.assertEqual(sorted(Path(call[-1]).name for call in cosign), sorted(["SHA256SUMS", *names]))
            for call in cosign:
                self.assertEqual(argument(call, "--certificate-identity-regexp"), RELEASE_IDENTITY)
                self.assertEqual(argument(call, "--certificate-oidc-issuer"), SIGSTORE_ISSUER)
            self.assertEqual(sorted(Path(call[3]).name for call in gh), sorted(["SHA256SUMS", *names, *sboms]))
            for call in gh:
                self.assertEqual(argument(call, "--repo"), "hjosugi/frost-build")
                self.assertEqual(
                    argument(call, "--signer-workflow"), "hjosugi/frost-build/.github/workflows/release.yml"
                )

            def flip(directory: Path) -> None:
                path = directory / names[0]
                data = bytearray(path.read_bytes())
                data[0] ^= 0xFF
                path.write_bytes(bytes(data))

            def rewrite_sums(directory: Path) -> None:
                flip(directory)
                (directory / "SHA256SUMS").write_text(
                    "".join(
                        f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n"
                        for name in names
                    )
                )

            def other_version_sbom(directory: Path) -> None:
                path = directory / sboms[1]
                path.write_text(path.read_text().replace(version, "0.0.1"))

            cases = {
                "tampered archive": (flip, "an archive does not match SHA256SUMS"),
                "rewritten checksums": (rewrite_sums, "the SHA256SUMS signature does not verify"),
                "missing archive signature": (
                    lambda directory: (directory / f"{names[1]}.sigstore.json").unlink(),
                    f"{names[1]} has no .sigstore.json signature bundle",
                ),
                "missing SBOM": (lambda directory: (directory / sboms[0]).unlink(), f"has no SBOM {sboms[0]}"),
                "SBOM of another version": (other_version_sbom, "is not this release's CycloneDX SBOM"),
                "missing provenance": (
                    lambda directory: (directory / f"frostbuild-v{version}.intoto.jsonl").unlink(),
                    f"has no frostbuild-v{version}.intoto.jsonl",
                ),
            }
            for label, (damage, message) in cases.items():
                with self.subTest(label):
                    directory = root / label.replace(" ", "-")
                    shutil.copytree(release, directory)
                    damage(directory)
                    result = verify(directory)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn(message, result.stderr)


if __name__ == "__main__":
    unittest.main()
