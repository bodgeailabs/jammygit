use jammygit::git::{Action, Repo, display_destination, parse_status};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    repo: Repo,
}

fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().into()
}

fn configure(root: &Path) {
    for (key, value) in [
        ("user.name", "Jammy Test"),
        ("user.email", "test@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", ".git/test-hooks"),
        ("core.autocrlf", "false"),
    ] {
        git(root, &["config", key, value]);
    }
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--initial-branch=main"]);
        configure(&root);
        let repo = Repo::discover(&root).unwrap();
        Self {
            _temp: temp,
            root,
            repo,
        }
    }
    fn write(&self, path: &str, content: &str) {
        fs::write(self.root.join(path), content).unwrap();
    }
    fn initial(&self) {
        self.write("file.txt", "initial\n");
        self.repo
            .execute_staging(&self.repo.plan(Action::StageAll).unwrap())
            .unwrap();
        self.commit("feat: initial");
    }
    fn commit(&self, message: &str) {
        self.repo
            .execute_interactive(&self.repo.plan(Action::Commit(message.into())).unwrap())
            .unwrap();
    }
    fn remote(&self) -> PathBuf {
        let path = self._temp.path().join("remote.git");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "--bare", "--initial-branch=main"]);
        git(
            &self.root,
            &["remote", "add", "origin", path.to_str().unwrap()],
        );
        path
    }
    fn push(&self) {
        self.repo
            .execute_interactive(&self.repo.plan(Action::Push).unwrap())
            .unwrap();
    }
    fn peer(&self) -> PathBuf {
        let path = self._temp.path().join("peer");
        git(
            self._temp.path(),
            &[
                "clone",
                self._temp.path().join("remote.git").to_str().unwrap(),
                path.to_str().unwrap(),
            ],
        );
        configure(&path);
        path
    }
}

#[test]
fn first_commit_uses_only_index_and_preserves_unstaged_changes() {
    let f = Fixture::new();
    f.write("file.txt", "staged\n");
    f.repo
        .execute_staging(&f.repo.plan(Action::StageAll).unwrap())
        .unwrap();
    f.write("file.txt", "not staged\n");
    f.write("other.txt", "untracked\n");
    f.commit("feat: first\n\nA multiline body with Unicode 🦀");
    assert_eq!(git(&f.root, &["show", "HEAD:file.txt"]), "staged");
    assert_eq!(
        fs::read_to_string(f.root.join("file.txt")).unwrap(),
        "not staged\n"
    );
    assert_eq!(f.repo.snapshot().unwrap().entries.len(), 2);
    assert!(git(&f.root, &["log", "-1", "--format=%B"]).contains("Unicode 🦀"));
}

#[test]
fn empty_commit_and_whitespace_message_are_rejected() {
    let f = Fixture::new();
    assert!(
        f.repo
            .plan(Action::Commit("feat: nothing".into()))
            .unwrap_err()
            .contains("Nothing staged")
    );
    assert!(
        f.repo
            .plan(Action::Commit(" \n ".into()))
            .unwrap_err()
            .contains("message")
    );
    f.initial();
    assert!(f.repo.plan(Action::Commit("feat: nothing".into())).is_err());
}

#[test]
fn unstage_before_first_commit_keeps_file_contents() {
    let f = Fixture::new();
    f.write("new.txt", "keep me");
    f.repo
        .execute_staging(&f.repo.plan(Action::StageAll).unwrap())
        .unwrap();
    f.write("new.txt", "also keep edits");
    let entry = f.repo.snapshot().unwrap().entries.remove(0);
    f.repo
        .execute_staging(&f.repo.plan(Action::Unstage(entry)).unwrap())
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.root.join("new.txt")).unwrap(),
        "also keep edits"
    );
    assert_eq!(f.repo.snapshot().unwrap().entries[0].index, b'?');
}

#[test]
fn stage_and_unstage_treat_pathspec_characters_literally() {
    let f = Fixture::new();
    f.initial();
    for path in [":(glob)*", "a b\nfile.txt", "--flag", "star*.txt"] {
        f.write(path, "safe");
    }
    let state = f.repo.snapshot().unwrap();
    let entry = state
        .entries
        .iter()
        .find(|e| e.path == Path::new(":(glob)*"))
        .unwrap()
        .clone();
    f.repo
        .execute_staging(&f.repo.plan(Action::Stage(entry)).unwrap())
        .unwrap();
    assert_eq!(
        f.repo
            .snapshot()
            .unwrap()
            .entries
            .iter()
            .filter(|e| e.staged())
            .count(),
        1
    );
    let entry = f
        .repo
        .snapshot()
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.staged())
        .unwrap();
    f.repo
        .execute_staging(&f.repo.plan(Action::Unstage(entry)).unwrap())
        .unwrap();
    assert!(f.root.join(":(glob)*").exists());
}

#[test]
fn staged_rename_unstages_both_paths_without_losing_work() {
    let f = Fixture::new();
    f.initial();
    git(&f.root, &["mv", "file.txt", "renamed file.txt"]);
    let entry = f.repo.snapshot().unwrap().entries.remove(0);
    assert_eq!(entry.original.as_deref(), Some(Path::new("file.txt")));
    assert!(f.repo.diff(Some(&entry), true).unwrap().contains("rename"));
    f.repo
        .execute_staging(&f.repo.plan(Action::Unstage(entry)).unwrap())
        .unwrap();
    assert_eq!(git(&f.root, &["diff", "--cached"]), "");
    assert_eq!(
        fs::read_to_string(f.root.join("renamed file.txt")).unwrap(),
        "initial\n"
    );
}

#[test]
fn stale_commit_confirmation_cannot_commit_a_changed_index() {
    let f = Fixture::new();
    f.initial();
    f.write("file.txt", "version one");
    git(&f.root, &["add", "file.txt"]);
    let plan = f
        .repo
        .plan(Action::Commit("feat: reviewed".into()))
        .unwrap();
    f.write("file.txt", "version two");
    git(&f.root, &["add", "file.txt"]);
    assert!(
        f.repo
            .execute_interactive(&plan)
            .unwrap_err()
            .contains("changed")
    );
    assert_eq!(git(&f.root, &["rev-list", "--count", "HEAD"]), "1");
}

#[cfg(unix)]
#[test]
fn failed_commit_hook_preserves_index_and_does_not_create_commit() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.initial();
    let hooks = f.root.join(".git/test-hooks");
    fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    fs::write(&hook, "#!/bin/sh\necho hook-rejected >&2\nexit 1\n").unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    f.write("file.txt", "staged changes");
    git(&f.root, &["add", "file.txt"]);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let plan = f
        .repo
        .plan(Action::Commit("feat: rejected".into()))
        .unwrap();
    assert!(f.repo.execute_interactive(&plan).is_err());
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
    assert!(f.repo.snapshot().unwrap().entries[0].staged());
}

#[test]
fn first_push_sets_upstream_and_respects_non_force_safety() {
    let f = Fixture::new();
    f.initial();
    let remote = f.remote();
    git(&f.root, &["config", "remote.origin.mirror", "true"]);
    // Configured wildcard refspecs and followTags must not expand the push.
    git(
        &f.root,
        &["config", "remote.origin.push", "+refs/heads/*:refs/heads/*"],
    );
    git(&f.root, &["config", "push.followTags", "true"]);
    git(&f.root, &["branch", "private-branch"]);
    git(&f.root, &["tag", "-a", "private-tag", "-m", "private"]);
    f.push();
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(&remote, &["for-each-ref", "--format=%(refname)"]),
        "refs/heads/main"
    );
    assert_eq!(
        f.repo.snapshot().unwrap().upstream.as_deref(),
        Some("origin/main")
    );
}

#[test]
fn rejected_push_does_not_overwrite_remote_history() {
    let f = Fixture::new();
    f.initial();
    let remote = f.remote();
    f.push();
    let peer = f.peer();
    fs::write(peer.join("peer.txt"), "peer").unwrap();
    git(&peer, &["add", "."]);
    git(&peer, &["commit", "-m", "feat: peer"]);
    git(&peer, &["push"]);
    let remote_head = git(&remote, &["rev-parse", "main"]);
    f.write("local.txt", "local");
    git(&f.root, &["add", "."]);
    f.commit("feat: local");
    let plan = f.repo.plan(Action::Push).unwrap();
    assert!(f.repo.execute_interactive(&plan).is_err());
    assert_eq!(git(&remote, &["rev-parse", "main"]), remote_head);
}

#[test]
fn pull_fast_forwards_and_ignores_rebase_autostash_settings() {
    let f = Fixture::new();
    f.initial();
    f.remote();
    f.push();
    let peer = f.peer();
    fs::write(peer.join("file.txt"), "remote change\n").unwrap();
    git(&peer, &["commit", "-am", "feat: remote"]);
    git(&peer, &["push"]);
    git(&f.root, &["config", "pull.rebase", "true"]);
    git(&f.root, &["config", "merge.autoStash", "true"]);
    git(&f.root, &["config", "rebase.autoStash", "true"]);
    f.repo
        .execute_interactive(&f.repo.plan(Action::Pull).unwrap())
        .unwrap();
    assert_eq!(
        git(&f.root, &["rev-parse", "HEAD"]),
        git(&peer, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        fs::read_to_string(f.root.join("file.txt")).unwrap(),
        "remote change\n"
    );
}

#[test]
fn pull_preserves_unrelated_staged_unstaged_and_untracked_changes() {
    let f = Fixture::new();
    f.initial();
    f.write("staged.txt", "original staged file\n");
    f.write("unstaged.txt", "original unstaged file\n");
    git(&f.root, &["add", "."]);
    f.commit("feat: local files");
    f.remote();
    f.push();
    let peer = f.peer();
    fs::write(peer.join("file.txt"), "incoming change\n").unwrap();
    git(&peer, &["commit", "-am", "feat: incoming"]);
    git(&peer, &["push"]);
    f.write("staged.txt", "staged version\n");
    git(&f.root, &["add", "staged.txt"]);
    f.write("staged.txt", "additional unstaged edits\n");
    f.write("unstaged.txt", "unstaged changes\n");
    f.write("untracked", "keep me\n");
    let index = git(&f.root, &["diff", "--cached", "--binary"]);
    let worktree = git(&f.root, &["diff", "--binary"]);
    f.repo
        .execute_interactive(&f.repo.plan(Action::Pull).unwrap())
        .unwrap();
    assert_eq!(
        git(&f.root, &["rev-parse", "HEAD"]),
        git(&peer, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        fs::read_to_string(f.root.join("file.txt")).unwrap(),
        "incoming change\n"
    );
    assert_eq!(git(&f.root, &["diff", "--cached", "--binary"]), index);
    assert_eq!(git(&f.root, &["diff", "--binary"]), worktree);
    assert_eq!(
        fs::read_to_string(f.root.join("untracked")).unwrap(),
        "keep me\n"
    );
    assert_eq!(git(&f.root, &["stash", "list"]), "");
}

#[test]
fn pull_lets_git_reject_overlapping_changes_without_losing_local_work() {
    for kind in ["staged", "unstaged", "untracked"] {
        let f = Fixture::new();
        f.initial();
        f.remote();
        f.push();
        let peer = f.peer();
        let path = if kind == "untracked" {
            "incoming.txt"
        } else {
            "file.txt"
        };
        fs::write(peer.join(path), "remote version\n").unwrap();
        git(&peer, &["add", "."]);
        git(&peer, &["commit", "-m", "feat: remote"]);
        git(&peer, &["push"]);
        f.write(path, "local version\n");
        if kind == "staged" {
            git(&f.root, &["add", path]);
        }
        let head = git(&f.root, &["rev-parse", "HEAD"]);
        let index = git(&f.root, &["diff", "--cached", "--binary"]);
        let plan = f.repo.plan(Action::Pull).unwrap();
        assert!(
            f.repo.execute_interactive(&plan).is_err(),
            "Git should reject {kind} overlap"
        );
        assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&f.root, &["diff", "--cached", "--binary"]), index);
        assert_eq!(
            fs::read_to_string(f.root.join(path)).unwrap(),
            "local version\n"
        );
        assert_eq!(git(&f.root, &["stash", "list"]), "");
    }
}

#[test]
fn up_to_date_pull_allows_dirty_worktree() {
    let f = Fixture::new();
    f.initial();
    f.remote();
    f.push();
    f.write("file.txt", "dirty");
    f.repo
        .execute_interactive(&f.repo.plan(Action::Pull).unwrap())
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.root.join("file.txt")).unwrap(),
        "dirty"
    );
}

#[test]
fn diverged_pull_refuses_to_merge_or_rebase() {
    let f = Fixture::new();
    f.initial();
    f.remote();
    f.push();
    let peer = f.peer();
    fs::write(peer.join("peer.txt"), "peer").unwrap();
    git(&peer, &["add", "."]);
    git(&peer, &["commit", "-m", "feat: peer"]);
    git(&peer, &["push"]);
    f.write("local.txt", "local");
    git(&f.root, &["add", "."]);
    f.commit("feat: local");
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    assert!(
        f.repo
            .execute_interactive(&f.repo.plan(Action::Pull).unwrap())
            .is_err()
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
    assert!(f.repo.snapshot().unwrap().operation.is_none());
    assert!(f.repo.snapshot().unwrap().entries.is_empty());
}

#[test]
fn missing_remote_upstream_and_detached_head_have_clear_errors() {
    let f = Fixture::new();
    f.initial();
    assert!(f.repo.plan(Action::Push).unwrap_err().contains("remote"));
    assert!(f.repo.plan(Action::Pull).unwrap_err().contains("upstream"));
    git(&f.root, &["checkout", "--detach"]);
    assert!(f.repo.plan(Action::Push).unwrap_err().contains("Detached"));
    assert!(f.repo.plan(Action::Pull).unwrap_err().contains("Detached"));
}

#[test]
fn upstream_with_different_branch_name_is_used_explicitly() {
    let f = Fixture::new();
    f.initial();
    let remote = f.remote();
    git(&f.root, &["push", "-u", "origin", "HEAD:review"]);
    f.write("file.txt", "second");
    git(&f.root, &["add", "."]);
    f.commit("feat: second");
    f.push();
    assert_eq!(
        git(&remote, &["rev-parse", "review"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
    let plan = f.repo.plan(Action::Pull).unwrap();
    assert!(plan.detail.contains("refs/heads/review"));
    f.repo.execute_interactive(&plan).unwrap();
}

#[test]
fn destination_change_invalidates_push_confirmation() {
    let f = Fixture::new();
    f.initial();
    f.remote();
    f.push();
    let plan = f.repo.plan(Action::Push).unwrap();
    git(
        &f.root,
        &["config", "branch.main.merge", "refs/heads/another"],
    );
    assert!(f.repo.validate(&plan).is_err());
}

#[test]
fn conflicts_and_in_progress_operations_block_commit_push_pull() {
    let f = Fixture::new();
    f.initial();
    f.remote();
    f.push();
    git(&f.root, &["checkout", "-b", "other"]);
    f.write("file.txt", "other\n");
    git(&f.root, &["commit", "-am", "feat: other"]);
    git(&f.root, &["checkout", "main"]);
    f.write("file.txt", "main\n");
    git(&f.root, &["commit", "-am", "feat: main"]);
    let status = Command::new("git")
        .current_dir(&f.root)
        .args(["merge", "other"])
        .output()
        .unwrap()
        .status;
    assert!(!status.success());
    let state = f.repo.snapshot().unwrap();
    assert_eq!(state.operation.as_deref(), Some("merge"));
    assert!(state.entries[0].conflict());
    for action in [
        Action::Commit("fix: conflict".into()),
        Action::Push,
        Action::Pull,
    ] {
        assert!(f.repo.plan(action).unwrap_err().contains("in progress"));
    }
}

#[test]
fn repository_discovery_works_from_subdirectories_and_linked_worktrees() {
    let f = Fixture::new();
    f.initial();
    let subdir = f.root.join("nested");
    fs::create_dir(&subdir).unwrap();
    assert_eq!(
        Repo::discover(&subdir)
            .unwrap()
            .root
            .canonicalize()
            .unwrap(),
        f.root.canonicalize().unwrap()
    );
    let linked = f._temp.path().join("linked");
    git(
        &f.root,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let repo = Repo::discover(&linked).unwrap();
    assert_eq!(repo.snapshot().unwrap().branch.as_deref(), Some("linked"));
    fs::write(linked.join("linked.txt"), "linked").unwrap();
    repo.execute_staging(&repo.plan(Action::StageAll).unwrap())
        .unwrap();
    repo.execute_interactive(&repo.plan(Action::Commit("feat: linked".into())).unwrap())
        .unwrap();
    assert!(f.repo.snapshot().unwrap().entries.is_empty());
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_paths_are_preserved_when_staging() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let f = Fixture::new();
    let path = PathBuf::from(OsString::from_vec(vec![b'f', 0xff, b'x']));
    fs::write(f.root.join(&path), "bytes").unwrap();
    let entry = f.repo.snapshot().unwrap().entries.remove(0);
    assert_eq!(entry.path, path);
    f.repo
        .execute_staging(&f.repo.plan(Action::Stage(entry)).unwrap())
        .unwrap();
    f.commit("feat: bytes");
    assert!(f.repo.snapshot().unwrap().entries.is_empty());
}

#[cfg(unix)]
#[test]
fn status_parser_preserves_non_utf8_bytes() {
    use std::os::unix::ffi::OsStrExt;
    // APFS disallows creating these names, but the parser must preserve Linux Git paths.
    let entries = parse_status(b"?? f\xffx\0").unwrap();
    assert_eq!(entries[0].path.as_os_str().as_bytes(), b"f\xffx");
}

#[test]
fn remote_url_change_invalidates_push_confirmation() {
    let f = Fixture::new();
    f.initial();
    f.remote();
    f.push();
    let plan = f.repo.plan(Action::Push).unwrap();
    git(
        &f.root,
        &["remote", "set-url", "origin", "/different-destination"],
    );
    assert!(f.repo.validate(&plan).is_err());
}

#[test]
fn multiple_push_urls_are_rejected_instead_of_sending_to_extra_destinations() {
    let f = Fixture::new();
    f.initial();
    let remote = f.remote();
    git(
        &f.root,
        &[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            remote.to_str().unwrap(),
        ],
    );
    git(
        &f.root,
        &[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            "/another-destination",
        ],
    );
    assert!(
        f.repo
            .plan(Action::Push)
            .unwrap_err()
            .contains("multiple push URLs")
    );
}

#[test]
fn nul_status_parser_handles_rename_newlines_and_rejects_bad_records() {
    let entries = parse_status(b"R  new\nname\0old name\0?? :(glob)*\0").unwrap();
    assert_eq!(entries[0].original.as_deref(), Some(Path::new("old name")));
    assert_eq!(entries[0].path, Path::new("new\nname"));
    assert_eq!(entries[1].path, Path::new(":(glob)*"));
    assert!(parse_status(b"broken\0").is_err());
    assert!(parse_status(b"R  new\0").is_err());
}

#[test]
fn remote_display_hides_credentials_and_query_tokens() {
    assert_eq!(
        display_destination(b"https://user:secret@example.com/team/repo.git?token=secret\n"),
        "https://example.com/team/repo.git"
    );
    assert_eq!(
        display_destination(b"git@example.com:team/repo.git\n"),
        "git@example.com:team/repo.git"
    );
}
