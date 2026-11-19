#!/usr/bin/env python3
"""Exercise the actual TUI in a Unix PTY with disposable Git repositories.

Usage: cargo build && python3 scripts/smoke_test.py [path-to-jammygit]
Only the Python standard library is required.
"""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def git(path, *args):
    return subprocess.check_output(
        ["git", "-C", str(path), *args], stderr=subprocess.PIPE, timeout=15
    ).decode().strip()


def configure(path):
    for key, value in {
        "user.name": "Jammy PTY Test",
        "user.email": "pty@example.invalid",
        "commit.gpgsign": "false",
        "core.hooksPath": ".git/test-hooks",
    }.items():
        git(path, "config", key, value)


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/jammygit").resolve()
    with tempfile.TemporaryDirectory(prefix="jammygit-pty-") as temp:
        root = Path(temp)
        work, remote, peer = root / "work", root / "remote.git", root / "peer"
        work.mkdir()
        remote.mkdir()
        git(work, "init", "--initial-branch=main")
        configure(work)
        git(remote, "init", "--bare", "--initial-branch=main")
        git(work, "remote", "add", "origin", str(remote))
        (work / "hello.txt").write_text("hello\n")
        # Real /dev/tty prompts verify embedded hooks can still ask for input.
        hooks = work / ".git/test-hooks"
        hooks.mkdir()
        hook = hooks / "pre-commit"
        hook.write_text('#!/bin/sh\nprintf "HOOK_CONFIRM: " > /dev/tty\nread answer < /dev/tty\n[ "$answer" = yes ]\n')
        hook.chmod(0o755)

        fd, slave_fd = pty.openpty()
        fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        pid = os.fork()
        if pid == 0:
            os.close(fd)
            os.login_tty(slave_fd)
            os.environ["TERM"] = "xterm-256color"
            # Keep a shell-like session leader alive to inspect terminal flags after exit.
            # macOS revokes the slave when its controlling session leader exits.
            status = subprocess.call([str(binary), str(work)])
            flags = termios.tcgetattr(0)[3]
            restored = bool(flags & termios.ICANON and flags & termios.ECHO)
            if restored:
                os.write(1, b"PTY_TERMINAL_RESTORED\n")
            os._exit(status if restored else 2)
        transcript = bytearray()
        finished = False

        def send(value):
            os.write(fd, value)

        def wait_for(marker, timeout=15, stay_in_app=False):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if stay_in_app:
                    assert b"\x1b[?1049l" not in transcript, "Push/pull left the TUI"
                    assert b"Press Enter to return" not in transcript, "Push/pull requested a return confirmation"
                if marker in transcript:
                    transcript.clear()
                    return
                ready, _, _ = select.select([fd], [], [], 0.1)
                if ready:
                    try:
                        data = os.read(fd, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not data:
                        break
                    transcript.extend(data)
            raise AssertionError(f"Missing {marker!r}; terminal tail: {bytes(transcript[-2500:])!r}")

        def wait_for_staging():
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if git(work, "diff", "--cached", "--name-only") == "hello.txt":
                    return
                ready, _, _ = select.select([fd], [], [], 0.1)
                if ready:
                    transcript.extend(os.read(fd, 65536))
            raise AssertionError("Stage-all did not update the index")

        try:
            wait_for(b"jammygit")
            send(b"a")
            wait_for_staging()
            send(b"c")
            wait_for(b"Commit staged changes")
            # Bracketed paste must insert the newline rather than submit early.
            send(b"\x1b[200~feat: PTY commit\n\nBody from pasted text.\x1b[201~")
            send(b"\r")
            wait_for(b"HOOK_CONFIRM:", stay_in_app=True)
            send(b"iyes\r")
            wait_for(b"Commit staged changes succeeded.", stay_in_app=True)
            assert git(work, "log", "-1", "--format=%B") == "feat: PTY commit\n\nBody from pasted text."
            push_hook = hooks / "pre-push"
            push_hook.write_text('#!/bin/sh\nprintf "PUSH_INPUT: " > /dev/tty\nread answer < /dev/tty\n[ "$answer" = yes ]\n')
            push_hook.chmod(0o755)
            send(b"P")
            # One key starts pushing; the hook prompt appears inside the app.
            wait_for(b"PUSH_INPUT:", stay_in_app=True)
            send(b"iyes\r")
            wait_for(b"Push branch succeeded.", stay_in_app=True)
            assert git(remote, "rev-parse", "main") == git(work, "rev-parse", "HEAD")

            git(root, "clone", str(remote), str(peer))
            configure(peer)
            (peer / "hello.txt").write_text("pulled through the TUI\n")
            git(peer, "commit", "-am", "feat: peer change")
            git(peer, "push")
            (work / "local-notes.txt").write_text("untracked work stays here\n")
            send(b"p")
            wait_for(b"Pull fast-forward succeeded.", stay_in_app=True)
            assert (work / "hello.txt").read_text() == "pulled through the TUI\n"
            assert (work / "local-notes.txt").read_text() == "untracked work stays here\n"
            # Earlier commits remain available after later pushes and pulls.
            send(b"\x1b[H")
            wait_for(b"feat: PTY commit", stay_in_app=True)
            send(b"\x1b[F")
            wait_for(b"PgUp scrolls back", stay_in_app=True)
            send(b"q")
            wait_for(b"PTY_TERMINAL_RESTORED")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
                ready, _, _ = select.select([fd], [], [], 0.05)
                if ready:
                    os.read(fd, 65536)
            assert finished, "TUI did not exit"
        finally:
            # Close our slave before reaping the controlling session leader;
            # macOS terminal revocation can otherwise stall failed-test cleanup.
            os.close(fd)
            os.close(slave_fd)
            if not finished:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
    print("PTY smoke test passed: in-app commit/push/pull, embedded hook input, session history, terminal restoration.")


if __name__ == "__main__":
    main()
