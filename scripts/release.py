#!/usr/bin/env python3
"""Version validation, reproducible release archives, and checksums. Python 3.11+."""
import argparse
import gzip
import hashlib
import io
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
)
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\Z")


def version(tag):
    current = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not TAG.fullmatch(tag) or tag != f"v{current}":
        raise ValueError(f"Release tag must be v{current}, matching Cargo.toml; got {tag!r}")
    return current


def package(tag, target, binary, out):
    current = version(tag)
    if target not in TARGETS:
        raise ValueError(f"Unsupported release target: {target}")
    binary = Path(binary).resolve()
    result = subprocess.run([str(binary), "--version"], check=True, capture_output=True, text=True, timeout=10)
    if result.stdout.strip() != f"jammygit {current}":
        raise ValueError("Binary version does not match the release tag")
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    archive = out / f"jammygit-{target}.tar.gz"
    with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode="w") as bundle:
            for name, path, mode in [("jammygit", binary, 0o755), ("LICENSE", ROOT / "LICENSE", 0o644), ("README.md", ROOT / "README.md", 0o644)]:
                data = path.read_bytes()
                member = tarfile.TarInfo(name)
                member.size, member.mode, member.mtime = len(data), mode, 0
                bundle.addfile(member, io.BytesIO(data))
    return archive


def prerelease(tag):
    return "-" in version(tag).split("+", 1)[0]


def checksums(directory):
    directory = Path(directory)
    expected = {f"jammygit-{target}.tar.gz" for target in TARGETS} | {"install.sh"}
    actual = {path.name for path in directory.iterdir() if path.is_file() and path.name != "SHA256SUMS"}
    if actual != expected:
        raise ValueError(f"Incomplete release: missing {sorted(expected - actual)}, unexpected {sorted(actual - expected)}")
    manifest = directory / "SHA256SUMS"
    manifest.write_text("".join(f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n" for name in sorted(expected)))
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("version", "prerelease", "package"):
        command = commands.add_parser(name)
        command.add_argument("--tag", required=True)
        if name == "package":
            command.add_argument("--target", required=True, choices=TARGETS)
            command.add_argument("--binary", required=True)
            command.add_argument("--out", required=True)
    commands.add_parser("checksums").add_argument("--directory", required=True)
    args = parser.parse_args()
    try:
        if args.command == "version":
            print(version(args.tag))
        elif args.command == "prerelease":
            print(str(prerelease(args.tag)).lower())
        elif args.command == "package":
            print(package(args.tag, args.target, args.binary, args.out))
        else:
            print(checksums(args.directory))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"release: {error}\n")


if __name__ == "__main__":
    main()
