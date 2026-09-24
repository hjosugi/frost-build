#!/usr/bin/env python3
"""Pack one platform's release archive, byte-for-byte reproducibly.

Every platform job in .github/workflows/release.yml used to pack with the
host's own tool: GNU tar on Linux, bsdtar on macOS, Compress-Archive on
Windows. Each of those records what the build machine happened to have —
file modification times from the checkout and the build, the runner's user
and group, directory-listing order, and gzip's own header timestamp — so two
builds of the same tag produced different archives even when the binaries
inside were identical, and a SHA256SUMS could never be reproduced.

This writes the same layout from explicit metadata instead:

    frostbuild-<tag>-<triple>/{frost,frostd}[.exe]
    frostbuild-<tag>-<triple>/{README.md,LICENSE,install.sh}
    frostbuild-<tag>-<triple>/share/...        (man pages and completions)

Entries are sorted by path; every timestamp is SOURCE_DATE_EPOCH (the release
commit's time); owner and group are 0 with empty names; modes are 0755 for
directories and executables and 0644 for everything else; gzip carries no file
name and a zero timestamp. Only regular files and directories are written,
which is also all `frost self-update` and install.sh accept.
"""

from __future__ import annotations

import argparse
import gzip
import io
import os
import re
import subprocess
import sys
import tarfile
import time
import zipfile
from pathlib import Path


TAG = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+$")
TRIPLE = re.compile(r"^[A-Za-z0-9_.-]+$")
# ZIP cannot represent a time before 1980.
ZIP_EPOCH = 315532800
# name -> executable. install.sh keeps the executable bit it has in git (and
# had in every earlier archive), fixed here rather than read from the checkout.
TOP_LEVEL_FILES = {"README.md": False, "LICENSE": False, "install.sh": True}


def source_date_epoch(explicit: int | None, repository: Path) -> int:
    if explicit is not None:
        return explicit
    environment = os.environ.get("SOURCE_DATE_EPOCH")
    if environment:
        return int(environment)
    # The commit's own timestamp: identical for every rebuild of the tag.
    output = subprocess.run(
        ["git", "-C", str(repository), "log", "-1", "--format=%ct"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    return int(output)


def collect(
    binary_dir: Path, binaries: list[str], repository: Path, assets: Path
) -> dict[str, tuple[Path | None, bool]]:
    """Map archive-relative paths to (source, executable); source None is a directory."""
    entries: dict[str, tuple[Path | None, bool]] = {}
    for name in binaries:
        source = binary_dir / name
        if not source.is_file():
            raise ValueError(f"missing release binary {source}")
        entries[name] = (source, True)
    for name, executable in TOP_LEVEL_FILES.items():
        source = repository / name
        if not source.is_file():
            raise ValueError(f"missing top-level release file {source}")
        entries[name] = (source, executable)
    share = assets / "share"
    if not share.is_dir():
        raise ValueError(f"{assets} has no share/ directory of generated documentation")
    entries["share"] = (None, False)
    for path in sorted(share.rglob("*")):
        relative = path.relative_to(assets).as_posix()
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError(f"{path} is not a regular file or directory")
        entries[relative] = (None, False) if path.is_dir() else (path, False)
    return entries


def tar_gz(root: str, entries: dict[str, tuple[Path | None, bool]], epoch: int) -> bytes:
    payload = io.BytesIO()
    with tarfile.open(fileobj=payload, mode="w", format=tarfile.PAX_FORMAT) as archive:

        def add(name: str, source: Path | None, executable: bool) -> None:
            info = tarfile.TarInfo(name)
            info.mtime = epoch
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            if source is None:
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                archive.addfile(info)
                return
            data = source.read_bytes()
            info.size = len(data)
            info.mode = 0o755 if executable else 0o644
            archive.addfile(info, io.BytesIO(data))

        add(root, None, False)
        for relative in sorted(entries):
            source, executable = entries[relative]
            add(f"{root}/{relative}", source, executable)
    compressed = io.BytesIO()
    # filename="" and mtime=0 keep the gzip header free of anything local.
    with gzip.GzipFile(filename="", mode="wb", fileobj=compressed, mtime=0, compresslevel=9) as stream:
        stream.write(payload.getvalue())
    return compressed.getvalue()


def zip_bytes(root: str, entries: dict[str, tuple[Path | None, bool]], epoch: int) -> bytes:
    stamp = time.gmtime(max(epoch, ZIP_EPOCH))[:6]
    payload = io.BytesIO()
    with zipfile.ZipFile(payload, "w") as archive:

        def add(name: str, source: Path | None, executable: bool) -> None:
            info = zipfile.ZipInfo(name + ("/" if source is None else ""), date_time=stamp)
            # Recorded as a Unix entry on every packing host, so the bytes do
            # not depend on which OS packed them and extractors that honour
            # modes (including `frost self-update`) see a plain file or dir.
            info.create_system = 3
            if source is None:
                info.external_attr = (0o040755 << 16) | 0x10
                archive.writestr(info, b"")
                return
            info.external_attr = (0o100755 if executable else 0o100644) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, source.read_bytes(), compresslevel=9)

        add(root, None, False)
        for relative in sorted(entries):
            source, executable = entries[relative]
            add(f"{root}/{relative}", source, executable)
    return payload.getvalue()


def package(
    tag: str,
    triple: str,
    binary_dir: Path,
    assets: Path,
    output: Path,
    archive_format: str,
    repository: Path,
    epoch: int,
) -> Path:
    if not TAG.fullmatch(tag):
        raise ValueError(f"{tag!r} is not a vX.Y.Z tag")
    if not TRIPLE.fullmatch(triple):
        raise ValueError(f"{triple!r} is not a target triple")
    suffix = ".exe" if archive_format == "zip" else ""
    binaries = [f"frost{suffix}", f"frostd{suffix}"]
    root = f"frostbuild-{tag}-{triple}"
    entries = collect(binary_dir, binaries, repository, assets)
    if archive_format == "tar.gz":
        data = tar_gz(root, entries, epoch)
    elif archive_format == "zip":
        data = zip_bytes(root, entries, epoch)
    else:
        raise ValueError(f"unsupported archive format {archive_format!r}")
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"{root}.{archive_format}"
    temporary = destination.with_name(destination.name + ".partial")
    temporary.write_bytes(data)
    os.replace(temporary, destination)
    return destination


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tag", required=True, help="release tag, vX.Y.Z")
    parser.add_argument("--triple", required=True, help="target triple the binaries were built for")
    parser.add_argument("--binary-dir", required=True, type=Path, help="directory holding frost and frostd")
    parser.add_argument("--assets", required=True, type=Path, help="generated release assets (holding share/)")
    parser.add_argument("--output", required=True, type=Path, help="directory to write the archive into")
    parser.add_argument("--format", choices=("tar.gz", "zip"), default="tar.gz")
    parser.add_argument("--repository", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument(
        "--source-date-epoch",
        type=int,
        help="timestamp for every entry (default: $SOURCE_DATE_EPOCH, else the HEAD commit time)",
    )
    arguments = parser.parse_args(argv)
    try:
        epoch = source_date_epoch(arguments.source_date_epoch, arguments.repository)
        written = package(
            arguments.tag,
            arguments.triple,
            arguments.binary_dir,
            arguments.assets,
            arguments.output,
            arguments.format,
            arguments.repository,
            epoch,
        )
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f"package_release.py: {error}", file=sys.stderr)
        return 2
    print(written)
    return 0


if __name__ == "__main__":
    sys.exit(main())
