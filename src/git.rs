use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub index: u8,
    pub worktree: u8,
    pub path: PathBuf,
    pub original: Option<PathBuf>,
}

impl Entry {
    pub fn staged(&self) -> bool {
        !matches!(self.index, b' ' | b'?')
    }

    pub fn unstaged(&self) -> bool {
        self.worktree != b' '
    }

    pub fn conflict(&self) -> bool {
        self.index == b'U'
            || self.worktree == b'U'
            || matches!((self.index, self.worktree), (b'A', b'A') | (b'D', b'D'))
    }

    pub fn label(&self) -> String {
        match &self.original {
            Some(old) => format!("{} → {}", display_path(old), display_path(&self.path)),
            None => display_path(&self.path),
        }
    }
}

// Keep paths as OS strings all the way into Git, including non-UTF-8 Unix names.
#[cfg(unix)]
fn os(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

pub fn display_path(path: &Path) -> String {
    safe_text(&path.to_string_lossy())
}

pub fn safe_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() {
            text.extend(c.escape_default());
        } else {
            text.push(c);
        }
    }
    text
}

/// Show a remote URL without embedding HTTP credentials or query tokens in the UI.
pub fn display_destination(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches('\n');
    let text = text.split(['?', '#']).next().unwrap_or(text);
    let redacted = if let Some((scheme, rest)) = text.split_once("://") {
        let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
        format!(
            "{scheme}://{}{path}",
            authority.rsplit('@').next().unwrap_or(authority)
        )
    } else {
        text.to_owned()
    };
    safe_text(&redacted)
}

pub fn parse_status(bytes: &[u8]) -> Result<Vec<Entry>> {
    let mut records = bytes.split(|b| *b == 0).peekable();
    let mut entries = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() && records.peek().is_none() {
            break;
        }
        if record.len() < 4 || record[2] != b' ' {
            return Err("Git returned an invalid status record; refresh before continuing.".into());
        }
        let original = if matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C') {
            let source = records
                .next()
                .filter(|v| !v.is_empty())
                .ok_or("Missing rename source")?;
            Some(PathBuf::from(os(source)))
        } else {
            None
        };
        entries.push(Entry {
            index: record[0],
            worktree: record[1],
            path: PathBuf::from(os(&record[3..])),
            original,
        });
    }
    Ok(entries)
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub entries: Vec<Entry>,
    pub upstream: Option<String>,
    pub counts: Option<(usize, usize)>,
    pub operation: Option<String>,
}

/// Porcelain v2 supplies branch metadata and paths in one stable, NUL-delimited read.
pub fn parse_snapshot(bytes: &[u8]) -> Result<Snapshot> {
    let mut state = Snapshot {
        branch: None,
        head: None,
        entries: Vec::new(),
        upstream: None,
        counts: None,
        operation: None,
    };
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix(b"# ") {
            let text = String::from_utf8_lossy(header);
            if let Some(v) = text.strip_prefix("branch.oid ") {
                if v != "(initial)" {
                    state.head = Some(v.into());
                }
            } else if let Some(v) = text.strip_prefix("branch.head ") {
                if v != "(detached)" {
                    state.branch = Some(v.into());
                }
            } else if let Some(v) = text.strip_prefix("branch.upstream ") {
                state.upstream = Some(v.into());
            } else if let Some(v) = text.strip_prefix("branch.ab ") {
                let mut parts = v.split_whitespace();
                let ahead = parts
                    .next()
                    .and_then(|v| v.strip_prefix('+'))
                    .and_then(|v| v.parse().ok());
                let behind = parts
                    .next()
                    .and_then(|v| v.strip_prefix('-'))
                    .and_then(|v| v.parse().ok());
                state.counts = ahead.zip(behind);
            }
            continue;
        }
        if let Some(path) = record.strip_prefix(b"? ") {
            state.entries.push(Entry {
                index: b'?',
                worktree: b'?',
                path: os(path).into(),
                original: None,
            });
            continue;
        }
        let fields = match record.first() {
            Some(b'1') => 9,
            Some(b'2') => 10,
            Some(b'u') => 11,
            _ => return Err("Invalid Git status record".into()),
        };
        let parts: Vec<_> = record.splitn(fields, |b| *b == b' ').collect();
        if parts.len() != fields || parts[1].len() != 2 || parts[fields - 1].is_empty() {
            return Err("Invalid Git status fields".into());
        }
        let original = if record[0] == b'2' {
            Some(
                os(records
                    .next()
                    .filter(|v| !v.is_empty())
                    .ok_or("Missing rename source")?)
                .into(),
            )
        } else {
            None
        };
        state.entries.push(Entry {
            index: if parts[1][0] == b'.' {
                b' '
            } else {
                parts[1][0]
            },
            worktree: if parts[1][1] == b'.' {
                b' '
            } else {
                parts[1][1]
            },
            path: os(parts[fields - 1]).into(),
            original,
        });
    }
    Ok(state)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Stage(Entry),
    Unstage(Entry),
    StageAll,
    UnstageAll,
    Commit(String),
    Push,
    Pull,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub action: Action,
    pub title: String,
    pub detail: String,
    pub args: Vec<OsString>,
    // Recheck immediately before execution so a stale confirmation is rejected.
    head: Option<String>,
    branch: Option<String>,
    index: Vec<u8>,
    entries: Vec<Entry>,
    destination: Option<Vec<u8>>,
}

impl Plan {
    pub fn with_commit_message(mut self, message: String) -> Result<Self> {
        if !matches!(self.action, Action::Commit(_)) {
            return Err("Not a commit review".into());
        }
        if message.trim().is_empty() {
            return Err("Enter a commit message first.".into());
        }
        if message.contains('\0') {
            return Err("Commit messages cannot contain NUL characters.".into());
        }
        self.args = strings(&["commit", "--cleanup=verbatim", "-m", &message]);
        self.action = Action::Commit(message);
        Ok(self)
    }
}

impl Repo {
    pub fn discover(path: &Path) -> Result<Self> {
        let result = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|e| format!("Cannot run Git: {e}"))?;
        let mut bytes = successful(result)?;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        Ok(Self {
            root: PathBuf::from(os(&bytes)),
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.root).args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "color.ui=false",
        ]);
        command
    }

    fn output(&self, args: &[&str]) -> Result<Vec<u8>> {
        successful(
            self.command()
                .args(args)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("Cannot run Git: {e}"))?,
        )
    }

    fn optional(&self, args: &[&str]) -> Result<Option<String>> {
        let out = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("Cannot run Git: {e}"))?;
        if out.status.success() {
            Ok(Some(
                String::from_utf8_lossy(&out.stdout)
                    .trim_end_matches('\n')
                    .into(),
            ))
        } else if out.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn config(&self, key: &str) -> Result<Option<String>> {
        self.optional(&["config", "--get", key])
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        let mut state = parse_snapshot(&self.output(&[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])?)?;
        let bytes = self.output(&["rev-parse", "--absolute-git-dir"])?;
        let git_dir = PathBuf::from(os(bytes.strip_suffix(b"\n").unwrap_or(&bytes)));
        for (file, label) in [
            ("MERGE_HEAD", "merge"),
            ("rebase-merge", "rebase"),
            ("rebase-apply", "rebase / apply"),
            ("CHERRY_PICK_HEAD", "cherry-pick"),
            ("REVERT_HEAD", "revert"),
            ("sequencer", "sequencer"),
            ("BISECT_START", "bisect"),
        ] {
            if git_dir.join(file).exists() {
                state.operation = Some(label.into());
                break;
            }
        }
        Ok(state)
    }

    pub fn log(&self) -> Result<String> {
        if self
            .optional(&["rev-parse", "--verify", "--quiet", "HEAD"])?
            .is_none()
        {
            return Ok("No commits yet. Stage a file, then press c.".into());
        }
        self.output(&["log", "-8", "--format=%h  %s", "--no-decorate"])
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    pub fn log_for(&self, state: &Snapshot) -> Result<String> {
        if state.head.is_none() {
            return Ok("No commits yet. Stage a file, then press c.".into());
        }
        self.output(&["log", "-8", "--format=%h  %s", "--no-decorate"])
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    pub fn diff(&self, entry: Option<&Entry>, staged: bool) -> Result<String> {
        if entry.is_some_and(|e| e.index == b'?') {
            return Ok(
                "Untracked file. Space stages it for review; c commits staged changes only.".into(),
            );
        }
        let mut command = self.command();
        command.args([
            "--no-optional-locks",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
        ]);
        if staged {
            command.arg("--cached");
        }
        command.arg("--");
        if let Some(entry) = entry {
            add_paths(&mut command, entry);
        }
        // Drain a bounded preview, then terminate Git rather than loading huge patches.
        use std::io::Read;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let read = child
            .stdout
            .take()
            .ok_or("Missing diff output")?
            .take(256 * 1024 + 1)
            .read_to_end(&mut bytes);
        let truncated = bytes.len() > 256 * 1024;
        if truncated || read.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        read.map_err(|e| e.to_string())?;
        if !truncated && !status.success() {
            return Err("Git diff failed. Press r to retry.".into());
        }
        bytes.truncate(256 * 1024);
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            text.push_str("\n… Preview limited to 256 KiB. Use git diff for the full patch.");
        }
        if text.is_empty() {
            text = "No changes in this view. Tab switches staged / unstaged.".into();
        }
        Ok(text)
    }

    fn upstream_target(&self, branch: &str) -> Result<Option<(String, String)>> {
        let remote = self.config(&format!("branch.{branch}.remote"))?;
        let merge = self.config(&format!("branch.{branch}.merge"))?;
        match (remote, merge) {
            (Some(remote), Some(reference)) if reference.starts_with("refs/heads/") => {
                Ok(Some((remote, reference)))
            }
            (None, None) => Ok(None),
            _ => Err("Incomplete branch upstream configuration. Set it with git branch --set-upstream-to.".into()),
        }
    }

    fn push_target(&self, branch: &str) -> Result<(String, String, bool)> {
        let upstream = self.upstream_target(branch)?;
        let override_remote = self
            .config(&format!("branch.{branch}.pushRemote"))?
            .or(self.config("remote.pushDefault")?);
        if let Some(remote) = override_remote {
            let reference = upstream
                .as_ref()
                .filter(|(r, _)| *r == remote)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| format!("refs/heads/{branch}"));
            return Ok((remote, reference, upstream.is_none()));
        }
        if let Some((remote, reference)) = upstream {
            return Ok((remote, reference, false));
        }
        let remotes = self.output(&["remote"])?;
        let remotes: Vec<_> = String::from_utf8_lossy(&remotes)
            .lines()
            .map(str::to_owned)
            .collect();
        let remote = if remotes.iter().any(|r| r == "origin") {
            "origin".into()
        } else if remotes.len() == 1 {
            remotes[0].clone()
        } else {
            return Err(
                "No unambiguous push remote. Configure origin or branch.<name>.pushRemote.".into(),
            );
        };
        Ok((remote, format!("refs/heads/{branch}"), true))
    }

    pub fn plan(&self, action: Action) -> Result<Plan> {
        let state = self.snapshot()?;
        if let Some(operation) = &state.operation
            && !matches!(
                action,
                Action::Stage(_) | Action::Unstage(_) | Action::StageAll | Action::UnstageAll
            )
        {
            return Err(format!(
                "A {operation} is in progress. Complete or abort it with Git before continuing."
            ));
        }
        let index = self.output(&[
            "diff",
            "--cached",
            "--raw",
            "-z",
            "--no-abbrev",
            "--no-ext-diff",
            "--no-textconv",
        ])?;
        let mut args: Vec<OsString> = Vec::new();
        let mut destination = None;
        let (title, detail) = match &action {
            Action::Stage(entry) | Action::Unstage(entry) => {
                if !state.entries.contains(entry) {
                    return Err("File state changed. Refresh and retry.".into());
                }
                if matches!(action, Action::Stage(_)) {
                    args.extend(strings(&["add", "-A", "--"]));
                } else if state.head.is_some() {
                    args.extend(strings(&["restore", "--staged", "--"]));
                } else {
                    args.extend(strings(&[
                        "rm",
                        "--cached",
                        "-r",
                        "-f",
                        "--ignore-unmatch",
                        "--",
                    ]));
                }
                if let Some(old) = &entry.original {
                    args.push(old.as_os_str().into());
                }
                args.push(entry.path.as_os_str().into());
                ("Update staging".into(), entry.label())
            }
            Action::StageAll => {
                args.extend(strings(&["add", "-A", "--", "."]));
                (
                    "Stage all changes".into(),
                    "All tracked changes and untracked files will be staged.".into(),
                )
            }
            Action::UnstageAll => {
                if state.head.is_some() {
                    args.extend(strings(&["restore", "--staged", "--", "."]));
                } else {
                    args.extend(strings(&[
                        "rm",
                        "--cached",
                        "-r",
                        "-f",
                        "--ignore-unmatch",
                        "--",
                        ".",
                    ]));
                }
                (
                    "Unstage all changes".into(),
                    "All staged changes will be unstaged. Working files are preserved.".into(),
                )
            }
            Action::Commit(message) => {
                if message.trim().is_empty() {
                    return Err("Enter a commit message first.".into());
                }
                if message.contains('\0') {
                    return Err("Commit messages cannot contain NUL characters.".into());
                }
                if state.entries.iter().any(Entry::conflict) {
                    return Err("Resolve and stage all conflicts before committing.".into());
                }
                if !state.entries.iter().any(Entry::staged) {
                    return Err(
                        "Nothing staged. Space stages the selected file; a stages all.".into(),
                    );
                }
                args.extend(strings(&["commit", "--cleanup=verbatim", "-m", message]));
                (
                    "Commit staged changes".into(),
                    format!(
                        "{} staged paths. Git hooks and signing run normally.",
                        state.entries.iter().filter(|e| e.staged()).count()
                    ),
                )
            }
            Action::Push | Action::Pull => {
                let branch = state.branch.as_deref().ok_or(
                    "Detached HEAD. Switch to a branch with Git before pushing or pulling.",
                )?;
                if state.head.is_none() {
                    return Err("Create the first commit before pushing or pulling.".into());
                }
                let (remote, reference, set_upstream) = if action == Action::Push {
                    self.push_target(branch)?
                } else {
                    let (r, b) = self.upstream_target(branch)?.ok_or("No upstream branch. Push this branch first or configure its upstream with Git.")?;
                    (r, b, false)
                };
                // Only named remotes (or Git's local '.' upstream) are accepted here.
                if remote != "." {
                    let urls = if action == Action::Push {
                        self.output(&["remote", "get-url", "--push", "--all", "--", &remote])?
                    } else {
                        self.output(&["remote", "get-url", "--", &remote])?
                    };
                    if action == Action::Push
                        && urls
                            .split(|c| *c == b'\n')
                            .filter(|v| !v.is_empty())
                            .count()
                            != 1
                    {
                        return Err("This remote has multiple push URLs. Use Git directly or configure a remote with one destination.".into());
                    }
                    destination = Some(urls);
                }
                self.output(&["check-ref-format", &reference])?;
                let destination_label = destination
                    .as_deref()
                    .map(display_destination)
                    .unwrap_or_else(|| "local repository".into());
                if action == Action::Push {
                    args.extend(strings(&[
                        "-c",
                        &format!("remote.{remote}.mirror=false"),
                        "push",
                        "--porcelain",
                        "--no-force",
                        "--no-follow-tags",
                        "--recurse-submodules=check",
                    ]));
                    if set_upstream {
                        args.push("--set-upstream".into());
                    }
                    args.extend(strings(&["--", &remote, &format!("HEAD:{reference}")]));
                    (
                        "Push branch".into(),
                        format!(
                            "{branch} → {remote}/{reference}\nDestination: {destination_label}\n{}",
                            if set_upstream {
                                "Sets upstream after success. Normal push; remote rejections are preserved."
                            } else {
                                "Normal push; remote rejections are preserved."
                            }
                        ),
                    )
                } else {
                    args.extend(strings(&[
                        "-c",
                        "merge.autoStash=false",
                        "-c",
                        "rebase.autoStash=false",
                        "pull",
                        "--ff-only",
                        "--no-rebase",
                        "--no-autostash",
                        "--no-squash",
                        "--",
                        &remote,
                        &reference,
                    ]));
                    (
                        "Pull fast-forward".into(),
                        format!(
                            "{remote}/{reference} → {branch}\nSource: {destination_label}\nStops if histories diverge. No automatic stash or merge commit."
                        ),
                    )
                }
            }
        };
        Ok(Plan {
            action,
            title,
            detail,
            args,
            head: state.head,
            branch: state.branch,
            index,
            entries: state.entries,
            destination,
        })
    }

    pub fn validate(&self, plan: &Plan) -> Result<()> {
        if &self.plan(plan.action.clone())? != plan {
            return Err(
                "Repository or destination changed since review. Refresh and retry.".into(),
            );
        }
        Ok(())
    }

    /// Uses the real terminal for hooks, GPG, SSH and credential helper prompts.
    pub fn execute_interactive(&self, plan: &Plan) -> Result<()> {
        self.validate(plan)?;
        let status = self
            .command()
            .args(&plan.args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("Cannot start Git: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} failed ({}). Review Git's output above before retrying.",
                plan.title, status
            ))
        }
    }

    pub fn toggle_all_staging(&self) -> Result<()> {
        let state = self.snapshot()?;
        if state.entries.is_empty() {
            return Ok(());
        }
        let action = if state.entries.iter().any(Entry::unstaged) {
            Action::StageAll
        } else {
            Action::UnstageAll
        };
        self.execute_staging(&self.plan(action)?)
    }

    pub fn execute_staging(&self, plan: &Plan) -> Result<()> {
        if !matches!(
            plan.action,
            Action::Stage(_) | Action::Unstage(_) | Action::StageAll | Action::UnstageAll
        ) {
            return Err("This operation needs the interactive terminal.".into());
        }
        self.validate(plan)?;
        successful(
            self.command()
                .args(&plan.args)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?,
        )
        .map(|_| ())
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn add_paths(command: &mut Command, entry: &Entry) {
    if let Some(old) = &entry.original {
        command.arg(old);
    }
    command.arg(&entry.path);
}

fn successful(output: Output) -> Result<Vec<u8>> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(if error.trim().is_empty() {
            format!("Git failed ({})", output.status)
        } else {
            error.trim().to_string()
        })
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn porcelain_v2_decodes_branch_rename_and_conflict_without_quoting_paths() {
        let bytes = b"# branch.oid abc123\0# branch.head feature/test\0# branch.upstream origin/feature/test\0# branch.ab +2 -3\0\
            1 M. N... 100644 100644 100644 a b path with spaces\0\
            2 R. N... 100644 100644 100644 a b R100 new\nname\0old name\0\
            u UU N... 100644 100644 100644 100644 a b c conflict\0? new file\0";
        let state = parse_snapshot(bytes).unwrap();
        assert_eq!(state.branch.as_deref(), Some("feature/test"));
        assert_eq!(state.upstream.as_deref(), Some("origin/feature/test"));
        assert_eq!(state.counts, Some((2, 3)));
        assert_eq!(state.entries[0].path, Path::new("path with spaces"));
        assert_eq!(state.entries[0].worktree, b' ');
        assert_eq!(
            state.entries[1].original.as_deref(),
            Some(Path::new("old name"))
        );
        assert!(state.entries[2].conflict());
        assert_eq!(state.entries[3].index, b'?');
    }

    #[test]
    fn porcelain_v2_handles_unborn_detached_and_malformed_records() {
        let unborn = parse_snapshot(b"# branch.oid (initial)\0# branch.head main\0").unwrap();
        assert!(unborn.head.is_none());
        assert_eq!(unborn.branch.as_deref(), Some("main"));
        let detached = parse_snapshot(b"# branch.oid abc\0# branch.head (detached)\0").unwrap();
        assert!(detached.branch.is_none());
        assert!(parse_snapshot(b"1 M. bad\0").is_err());
    }
}
