# jammygit

A small Git TUI built with [Ratatui](https://ratatui.rs/). Stage, review, commit, push, pull. Uses the installed Git CLI and your existing Git configuration, credentials, hooks, and signing setup.

The interface takes its blue (`#234e91`) from Initium. Changed files sit in a central conversation-style view, with commit input in a composer below. Use the arrow keys to select a file and Enter to slide in its diff from the right. Git activity appears in the same view; commits, pushes, pulls, and their prompts all stay inside the app.

![Jammygit workspace with sample changes](docs/preview.png)

## Run

Requires a current stable Rust toolchain, Git 2.23+, and an interactive terminal (at least 64 × 18; 100 × 30 recommended).

```sh
cd /Users/sam/code/initiumailab/jammygit
cargo run --release -- /path/to/your/repository
```

The path is optional; it defaults to the current directory. Subdirectories and linked Git worktrees work too. To install the command:

```sh
cargo install --path . --locked
jammygit /path/to/your/repository
```

## Keys

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Select a changed file |
| `Enter` / Right | Slide in the selected file's diff |
| `Esc` / Left | Close the diff drawer |
| `Space` | Stage selected file; unstage if fully staged |
| `u` | Unstage selected file, preserving working files |
| `a` | Confirm staging all changes, including untracked files |
| `Tab` | Switch staged / unstaged diff |
| `v` | Switch selected-file / all-files diff |
| `J` / `K`, Page Down / Up | Scroll the diff |
| `h` / `l` | Pan the diff horizontally |
| `c` | Write a message and commit staged changes |
| `P` | Push the current branch immediately, inside the app |
| `p` | Pull immediately inside the app, fast-forward only |
| `o` | Show / hide the most recent Git activity |
| `g` | Recent commits |
| `r` | Refresh |
| `?` | Help |
| `q` / `Ctrl+C` | Quit |

In the commit composer, `Enter` commits, `Ctrl+J` or `Alt+Enter` inserts a newline, and `Esc` returns to the file list with the draft saved. Left/right, Home/End, Backspace/Delete, and `Ctrl+U` edit the message. Bracketed paste supports multiline messages in compatible terminals. Failed commits keep the draft for retry; successful commits clear it.

Each file has a readable status: staged, unstaged, partly staged, untracked, or conflicted. A filled marker indicates staged content. Staging a file stages all its current changes; partial staging belongs in `git add -p` outside the app. The diff drawer automatically picks the staged view for files with only staged changes. On narrow terminals the drawer occupies the full content width; Esc returns to the file list.

## Commit, push, and pull behavior

- **Commit:** only the existing index is committed. No automatic staging, amend, or hook/signing bypass. The staged index is checked again when you submit the message. Empty messages and unresolved conflicts are rejected.
- **Push:** starts immediately with `P`. The output pane shows the remote, branch, and destination URL (with URL credentials hidden). The app sends one explicit branch refspec, without force, extra tags, wildcard refspecs, or mirror behavior. Remote rejections remain failures. Multiple push URLs are rejected. A first push sets the upstream after success.
- **Pull:** requires an upstream and runs an explicit fast-forward-only pull with rebase, squash, and autostash disabled. Local staged, unstaged, and untracked changes are allowed; Git decides whether the incoming update can be applied safely. If incoming changes would overwrite local work, Git stops and its error stays visible in the app. Divergent history also stops with Git's error; resolve it using your preferred Git workflow outside the app.
- **Commit/push/pull output and prompts:** run in an embedded terminal while the TUI stays open and responsive, with no extra confirmation or return prompt. Git activity remains visible after completion; `o` hides/reopens it, and Enter opens a selected file's diff. If Git, SSH, signing, or a hook asks for input, press `i`, type your response, and press Enter. `Esc` leaves input mode; `Ctrl+C` interrupts Git while keeping the app open. Other mutations and quitting are blocked until Git finishes. Output has a bounded queue and 1,000 lines of scrollback; Page Up/Down or `K`/`J` scroll it. Success appears only after Git exits successfully; failures retain their Git output.
- **State checks:** repository state and the resolved remote URL are rechecked immediately before execution. Stale confirmations are rejected. Avoid running simultaneous Git mutations in another process; preflight checks cannot lock out external Git commands.

Push target selection honors `branch.<name>.pushRemote`, then `remote.pushDefault`, then the branch upstream. Without an upstream or override, it uses `origin`, or the sole configured remote. An existing upstream supplies its remote branch name; a different push remote uses the local branch name. Pull always uses the configured upstream. Custom `push.default` policies and configured push refspecs are replaced by this explicit single-branch behavior.

Status refreshes in the background every three seconds while no dialog or Git operation is active, and again after an operation finishes. Ahead/behind counts use local tracking refs; there is no automatic network traffic. Pull fetches current remote state. Large diff previews are limited to 256 KiB. Untracked files must be staged to preview their patch.

Navigation and typing do not run Git commands on the UI thread. Status, diff reads, staging, and operation preparation use background workers. Diffs are loaded only while their drawer is open, briefly coalesced during fast navigation, and cached for the current repository view. Late responses cannot replace a newer selection. Only visible file and diff rows are rendered, and unchanged idle screens are not redrawn.

This first version leaves branch switching, conflict resolution, merge/rebase continuation, stashing, and force pushes to Git. In-progress merge, rebase, cherry-pick, revert, sequencer, and bisect operations block commit/push/pull with an explanation. You can still stage conflict resolutions.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 scripts/smoke_test.py
```

To measure navigation handlers and rendering with 5,000 changed files:

```sh
cargo test --release interaction_benchmark -- --ignored --nocapture
```

This reports handler and Ratatui frame timings, not network-operation duration or a terminal emulator's display latency.

The UI lives in `src/ui.rs`. To regenerate the sample screenshot on macOS:

```sh
JAMMYGIT_PREVIEW_DIR=/tmp/jammygit-preview cargo test export_preview -- --ignored
swift scripts/render_preview.swift /tmp/jammygit-preview/workspace.cells docs/preview.png
swift scripts/render_preview.swift /tmp/jammygit-preview/diff.cells docs/diff-preview.png
```

This renders the actual Ratatui cell buffer, including its colors, into a PNG. The preview exporter also produces compact, open-drawer, commit-composer, and Git-activity screens for visual review.

Workflow tests use disposable repositories and local bare remotes. They cover staged-only and first commits, Unicode messages, rejected hooks, unusual filenames, renames, stale confirmations, push destinations, rejected pushes, fast-forward and diverged pulls, conflicts, and linked worktrees. Embedded-terminal tests cover commits, prompts, input, resizing, failure output, and interruption. UI tests cover arrow-key selection, drawer controls, and draft retention after a rejected commit. The Unix PTY smoke test (Python 3.11+) drives the actual TUI through stage → multiline commit with an embedded hook prompt → one-key push with another embedded prompt → one-key pull. It verifies all three operations stay in the TUI and the terminal is restored on exit. Real hosting-provider authentication and signing depend on your local Git setup.
