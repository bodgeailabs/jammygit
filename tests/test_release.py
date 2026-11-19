"""Exercise release artifacts and the real Bash installer without network or home writes."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

MOCK = r'''
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
name = pathlib.Path(sys.argv[0]).name
with open(os.environ['MOCK_LOG'], 'a') as log: log.write(json.dumps([name, *args]) + '\n')
assets = pathlib.Path(os.environ['MOCK_ASSETS'])
if name == 'uname':
    print(os.environ.get('MOCK_OS', 'Linux') if args == ['-s'] else os.environ.get('MOCK_ARCH', 'x86_64'))
elif name == 'gh':
    if args[:2] == ['auth', 'status']: sys.exit(0 if os.environ.get('MOCK_AUTH') == '1' else 1)
    if os.environ.get('MOCK_PUBLISH'):
        operation = args[1]
        if operation == 'view':
            state = os.environ.get('MOCK_RELEASE_STATE', 'missing')
            if state == 'missing': sys.exit(1)
            print('true' if state == 'draft' else 'false')
        elif operation == os.environ.get('MOCK_PUBLISH_FAIL'): sys.exit(1)
        sys.exit(0)
    if args[:2] == ['release', 'view']: print('v0.1.0')
    elif args[:2] == ['release', 'download']:
        directory = pathlib.Path(args[args.index('--dir')+1])
        for i, value in enumerate(args):
            if value == '--pattern': shutil.copyfile(assets / args[i+1], directory / args[i+1])
    else: sys.exit(2)
elif name == 'curl':
    if '--write-out' in args: print('https://github.com/initiumailab/jammygit/releases/tag/v0.1.0', end='')
    else:
        failure = os.environ.get('MOCK_DOWNLOAD_FAIL')
        if failure and (failure != 'archive' or args[-1].endswith('.tar.gz')): print('mock HTTP 404', file=sys.stderr); sys.exit(22)
        output = pathlib.Path(args[args.index('--output')+1])
        shutil.copyfile(assets / args[-1].rsplit('/', 1)[-1], output)
'''


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="jammygit-installer-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        # Fake 0.1.0 assets belong to a fake project, independent of the real
        # package version. Keep tag and binary validation enabled in the tests.
        (self.root / "Cargo.toml").write_text('[package]\nversion = "0.1.0"\n')
        for name in ("LICENSE", "README.md"):
            shutil.copyfile(ROOT / name, self.root / name)
        project = mock.patch.object(release, "ROOT", self.root)
        project.start()
        self.addCleanup(project.stop)
        self.assets = self.root / "assets"
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.binary = self.root / "binary"
        self.binary.write_text("#!/bin/sh\nprintf 'jammygit 0.1.0\\n'\n")
        self.binary.chmod(0o755)
        for target in release.TARGETS:
            release.package("v0.1.0", target, self.binary, self.assets)
        shutil.copyfile(ROOT / "install.sh", self.assets / "install.sh")
        release.checksums(self.assets)
        for name in ("uname", "curl", "gh"):
            path = self.tools / name
            path.write_text(f"#!{sys.executable}\n" + MOCK)
            path.chmod(0o755)
        self.destination = self.root / "installed bin"
        self.log = self.root / "requests.jsonl"
        self.env = {
            **os.environ,
            "PATH": f"{self.tools}:{os.environ['PATH']}",
            "HOME": str(self.root / "home"),
            "TMPDIR": str(self.root),
            "MOCK_ASSETS": str(self.assets),
            "MOCK_LOG": str(self.log),
            "MOCK_AUTH": "0",
            "NO_COLOR": "1",
        }
        for key in ("JAMMYGIT_INSTALL_DIR", "JAMMYGIT_REPO"):
            self.env.pop(key, None)

    def install(self, *args, env=None):
        return subprocess.run(
            ["bash", "-s", "--", "--bin-dir", str(self.destination), *args],
            input=(ROOT / "install.sh").read_text(), capture_output=True, text=True,
            env={**self.env, **(env or {})}, timeout=15,
        )

    def requests(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_package_is_reproducible_and_contains_executable_binary(self):
        target = release.TARGETS[0]
        first = (self.assets / f"jammygit-{target}.tar.gz").read_bytes()
        second = release.package("v0.1.0", target, self.binary, self.root / "again").read_bytes()
        self.assertEqual(first, second)
        with tarfile.open(self.assets / f"jammygit-{target}.tar.gz") as bundle:
            self.assertEqual(bundle.getnames(), ["jammygit", "LICENSE", "README.md"])
            self.assertEqual(bundle.getmember("jammygit").mode, 0o755)

    def test_tag_and_binary_version_must_match(self):
        with self.assertRaises(ValueError): release.version("v9.9.9")
        self.binary.write_text("#!/bin/sh\necho 'jammygit 9.9.9'\n")
        with self.assertRaisesRegex(ValueError, "Binary version does not match"):
            release.package("v0.1.0", release.TARGETS[0], self.binary, self.assets)

    def test_version_bumps_use_the_fixture_manifest_and_binary(self):
        for current in ("0.1.2", "1.2.3", "2.0.0-rc.1+build-7"):
            with self.subTest(version=current):
                (self.root / "Cargo.toml").write_text(f'[package]\nversion = "{current}"\n')
                self.binary.write_text(f"#!/bin/sh\necho 'jammygit {current}'\n")
                self.assertEqual(release.version(f"v{current}"), current)
                with self.assertRaisesRegex(ValueError, "Release tag must be"):
                    release.version("v0.1.0")
                archive = release.package(f"v{current}", release.TARGETS[0], self.binary, self.assets)
                with tarfile.open(archive) as bundle:
                    self.assertEqual(bundle.extractfile("jammygit").read(), self.binary.read_bytes())

    def test_prerelease_detection_ignores_hyphens_in_build_metadata(self):
        manifest = self.root / "manifest"
        manifest.mkdir()
        with mock.patch.object(release, "ROOT", manifest):
            for version, expected in [("0.2.0-rc.1", True), ("0.2.0+build-1", False), ("0.2.0", False)]:
                (manifest / "Cargo.toml").write_text(f'[package]\nversion = "{version}"\n')
                self.assertEqual(release.prerelease(f"v{version}"), expected)

    def test_checksums_require_every_platform(self):
        (self.assets / f"jammygit-{release.TARGETS[0]}.tar.gz").unlink()
        with self.assertRaises(ValueError): release.checksums(self.assets)

    def test_checksum_manifest_matches_packaged_bytes(self):
        for line in (self.assets / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split("  ")
            self.assertEqual(digest, hashlib.sha256((self.assets / name).read_bytes()).hexdigest())

    def publish(self, state="missing", failure=""):
        scripts = self.root / "scripts"
        scripts.mkdir(exist_ok=True)
        helper = scripts / "release.py"
        if not helper.exists(): shutil.copyfile(ROOT / "scripts/release.py", helper)
        dist = self.root / "dist"
        if not dist.exists(): dist.symlink_to(self.assets, target_is_directory=True)
        return subprocess.run(
            ["bash", str(ROOT / "scripts/publish-release.sh")], cwd=self.root,
            capture_output=True, text=True, timeout=10,
            env={**self.env, "MOCK_PUBLISH": "1", "MOCK_RELEASE_STATE": state, "MOCK_PUBLISH_FAIL": failure, "RELEASE_TAG": "v0.1.0", "RELEASE_REPO": "initiumailab/jammygit"},
        )

    def test_publish_uploads_a_complete_draft_before_publishing(self):
        result = self.publish()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = [r for r in self.requests() if r[0] == "gh"]
        self.assertEqual([r[2] for r in commands], ["view", "create", "edit"])
        self.assertIn("--draft", commands[1])
        self.assertIn("--verify-tag", commands[1])
        self.assertIn("--draft=false", commands[2])
        for target in release.TARGETS:
            self.assertIn(f"dist/jammygit-{target}.tar.gz", commands[1])
        self.assertIn("dist/SHA256SUMS", commands[1])
        self.assertIn("dist/install.sh", commands[1])

    def test_publish_can_resume_draft_upload(self):
        result = self.publish("draft")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([r[2] for r in self.requests() if r[0] == "gh"], ["view", "upload", "edit"])

    def test_publish_never_replaces_a_published_release(self):
        result = self.publish("published")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([r[2] for r in self.requests() if r[0] == "gh"], ["view"])

    def test_failed_upload_does_not_publish_draft(self):
        result = self.publish("draft", "upload")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("edit", [r[2] for r in self.requests() if r[0] == "gh"])

    def test_platforms_install_using_pinned_latest_release(self):
        for system, arch, target in [
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
            ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
        ]:
            with self.subTest(system=system, arch=arch):
                result = self.install(env={"MOCK_OS": system, "MOCK_ARCH": arch})
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(target, result.stdout)
                self.assertIn("Ready", result.stdout)
                self.assertNotIn("\x1b", result.stdout)
                self.assertEqual(subprocess.check_output([str(self.destination / "jammygit"), "--version"], text=True).strip(), "jammygit 0.1.0")
                requests = [r for r in self.requests() if r[0] == "curl" and "--write-out" not in r]
                self.assertTrue(all("/download/v0.1.0/" in r[-1] for r in requests))

    def test_explicit_version_skips_latest_lookup(self):
        result = self.install("--version", "0.1.0")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(any("--write-out" in r for r in self.requests()))

    def test_private_release_uses_authenticated_github_cli(self):
        result = self.install(env={"MOCK_AUTH": "1"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(any(r[:3] == ["gh", "release", "download"] for r in self.requests()))
        self.assertFalse(any(r[0] == "curl" for r in self.requests()))

    def test_macos_shasum_fallback_without_gnu_coreutils(self):
        # GNU tar invokes gzip via PATH; bsdtar on macOS decompresses internally.
        for name in ["bash", "tar", "gzip", "awk", "git", "mkdir", "rm", "cp", "chmod", "mv", "mktemp", "cat", "sleep", "shasum"]:
            location = shutil.which(name)
            if not location: self.skipTest(f"{name} is unavailable")
            (self.tools / name).symlink_to(location)
        result = self.install(env={"PATH": str(self.tools)})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_bad_checksum_preserves_existing_install(self):
        self.destination.mkdir()
        installed = self.destination / "jammygit"
        installed.write_text("old version")
        archive = self.assets / "jammygit-x86_64-unknown-linux-musl.tar.gz"
        archive.write_bytes(archive.read_bytes() + b"corruption")
        result = self.install("--version", "v0.1.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Verifying archive failed", result.stderr)
        self.assertEqual(installed.read_text(), "old version")

    def test_missing_checksum_refuses_install(self):
        (self.assets / "SHA256SUMS").write_text("")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.destination / "jammygit").exists())

    def test_failed_extraction_never_executes_or_installs_partial_binary(self):
        self.destination.mkdir()
        installed = self.destination / "jammygit"
        installed.write_text("old version")
        marker = self.root / "partial-binary-ran"
        partial = f"#!/bin/sh\necho 'jammygit 0.1.0'\nprintf ran > {shlex.quote(str(marker))}\n"
        tar = self.tools / "tar"
        tar.write_text(f"#!{sys.executable}\nimport sys\nsys.stdout.write({partial!r})\nsys.stderr.write('mock extraction failed\\n')\nsys.exit(2)\n")
        tar.chmod(0o755)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mock extraction failed", result.stderr)
        self.assertNotIn("version mismatch", result.stderr)
        self.assertFalse(marker.exists())
        self.assertEqual(installed.read_text(), "old version")

    def test_failed_copy_preserves_existing_install(self):
        self.destination.mkdir()
        installed = self.destination / "jammygit"
        installed.write_text("old version")
        cp = self.tools / "cp"
        cp.write_text("#!/bin/sh\necho 'mock copy failed' >&2\nexit 1\n")
        cp.chmod(0o755)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mock copy failed", result.stderr)
        self.assertEqual(installed.read_text(), "old version")
        self.assertFalse(list(self.destination.glob(".jammygit.*")))

    def test_binary_version_mismatch_preserves_existing_install(self):
        # Modify an otherwise valid archive and regenerate its trusted manifest.
        self.binary.write_text("#!/bin/sh\necho 'jammygit 9.9.9'\n")
        target = self.assets / "jammygit-x86_64-unknown-linux-musl.tar.gz"
        with tarfile.open(target, "w:gz") as bundle: bundle.add(self.binary, arcname="jammygit")
        release.checksums(self.assets)
        self.destination.mkdir()
        installed = self.destination / "jammygit"
        installed.write_text("old version")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("version mismatch", result.stderr)
        self.assertEqual(installed.read_text(), "old version")
        self.assertFalse(list(self.destination.glob(".jammygit.*")))

    def test_failed_download_refuses_install(self):
        result = self.install(env={"MOCK_DOWNLOAD_FAIL": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Downloading release failed", result.stderr)
        self.assertFalse((self.destination / "jammygit").exists())

    def test_failed_archive_download_stops_before_requesting_checksums(self):
        result = self.install(env={"MOCK_DOWNLOAD_FAIL": "archive"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Downloading release failed", result.stderr)
        self.assertFalse(any(r[0] == "curl" and r[-1].endswith("SHA256SUMS") for r in self.requests()))

    def test_unsupported_platform_and_invalid_arguments_do_not_download(self):
        for args, env in [((), {"MOCK_OS": "FreeBSD"}), ((), {"MOCK_ARCH": "armv7l"}), (("--version", "../../bad"), {}), (("--repo", "bad;command"), {})]:
            with self.subTest(args=args, env=env):
                result = self.install(*args, env=env)
                self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(r[0] == "curl" for r in self.requests()))

    def test_help_performs_no_download(self):
        result = self.install("--help")
        self.assertEqual(result.returncode, 0)
        self.assertIn("--bin-dir", result.stdout)
        self.assertEqual(self.requests(), [])

    @unittest.skipUnless(os.name == "posix", "terminal styling uses a Unix PTY")
    def test_tty_progress_and_cleanup(self):
        import pty
        master, slave = pty.openpty()
        env = {**self.env, "TERM": "xterm-256color"}
        env.pop("NO_COLOR")
        process = subprocess.Popen(["bash", str(ROOT / "install.sh"), "--bin-dir", str(self.destination)], stdin=subprocess.DEVNULL, stdout=slave, stderr=slave, env=env)
        os.close(slave)
        output = bytearray()
        try:
            while process.poll() is None:
                if select.select([master], [], [], 0.1)[0]:
                    try: output.extend(os.read(master, 65536))
                    except OSError: break
            self.assertEqual(process.wait(timeout=10), 0, output.decode(errors="replace"))
        finally:
            if process.poll() is None: process.kill(); process.wait()
            os.close(master)
        self.assertIn(b"\x1b[38;2;", output)
        self.assertTrue((self.destination / "jammygit").is_file())
        self.assertFalse(list(self.destination.glob(".jammygit.*")))
        self.assertFalse(list(self.root.glob("jammygit-install.*")))


if __name__ == "__main__":
    unittest.main()
