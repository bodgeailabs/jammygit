use jammygit::{
    git::{Action, Repo},
    network::{Event, Job},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    remote: PathBuf,
    repo: Repo,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        let remote = temp.path().join("remote.git");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&remote).unwrap();
        git(&root, &["init", "--initial-branch=main"]);
        git(&remote, &["init", "--bare", "--initial-branch=main"]);
        for (key, value) in [
            ("user.name", "Network Test"),
            ("user.email", "network@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/test-hooks"),
        ] {
            git(&root, &["config", key, value]);
        }
        git(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        fs::write(root.join("file"), "initial").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "feat: initial"]);
        let repo = Repo::discover(&root).unwrap();
        Self {
            _temp: temp,
            root,
            remote,
            repo,
        }
    }

    #[cfg(unix)]
    fn hook(&self, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        let hooks = self.root.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        let path = hooks.join("pre-push");
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn start(&self, action: Action) -> Job {
        Job::start(&self.repo, &self.repo.plan(action).unwrap(), 24, 80).unwrap()
    }
}

fn finish(job: &mut Job) -> (String, Result<(), String>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        match job.poll() {
            Some(Event::Output(bytes)) => output.extend(bytes),
            Some(Event::Finished(result)) => {
                return (String::from_utf8_lossy(&output).into_owned(), result);
            }
            Some(Event::InputError(error)) => panic!("{error}"),
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("Git job timed out: {}", String::from_utf8_lossy(&output));
}

#[test]
fn embedded_push_and_pull_report_actual_results() {
    let f = Fixture::new();
    let (output, result) = finish(&mut f.start(Action::Push));
    result.unwrap();
    assert!(output.contains("main"));
    assert_eq!(
        git(&f.remote, &["rev-parse", "main"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        f.repo.snapshot().unwrap().upstream.as_deref(),
        Some("origin/main")
    );
    let (output, result) = finish(&mut f.start(Action::Pull));
    result.unwrap();
    assert!(output.contains("Already up to date"));
}

#[test]
fn embedded_commit_uses_only_staged_content_and_preserves_the_message() {
    let f = Fixture::new();
    fs::write(f.root.join("file"), "staged content").unwrap();
    git(&f.root, &["add", "file"]);
    fs::write(f.root.join("file"), "unstaged content").unwrap();
    let (output, result) = finish(&mut f.start(Action::Commit(
        "feat: embedded commit\n\nUnicode body 🦀".into(),
    )));
    result.unwrap();
    assert!(output.contains("feat: embedded commit"));
    assert_eq!(git(&f.root, &["show", "HEAD:file"]), "staged content");
    assert_eq!(
        fs::read_to_string(f.root.join("file")).unwrap(),
        "unstaged content"
    );
    assert!(git(&f.root, &["log", "-1", "--format=%B"]).contains("Unicode body 🦀"));
}

#[cfg(unix)]
#[test]
fn rejected_embedded_commit_keeps_head_and_index() {
    let f = Fixture::new();
    f.hook("#!/bin/sh\necho COMMIT_REJECTED >&2\nexit 1\n");
    fs::rename(
        f.root.join(".git/test-hooks/pre-push"),
        f.root.join(".git/test-hooks/pre-commit"),
    )
    .unwrap();
    fs::write(f.root.join("file"), "staged content").unwrap();
    git(&f.root, &["add", "file"]);
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let (output, result) = finish(&mut f.start(Action::Commit("feat: rejected".into())));
    assert!(result.is_err());
    assert!(output.contains("COMMIT_REJECTED"));
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
    assert!(f.repo.snapshot().unwrap().entries[0].staged());
}

#[cfg(unix)]
#[test]
fn embedded_push_captures_rejection_without_reporting_success() {
    let f = Fixture::new();
    f.hook("#!/bin/sh\necho PUSH_REJECTED >&2\nexit 1\n");
    let (output, result) = finish(&mut f.start(Action::Push));
    assert!(result.is_err());
    assert!(output.contains("PUSH_REJECTED"));
    assert!(git(&f.remote, &["for-each-ref"]).is_empty());
}

#[cfg(unix)]
#[test]
fn embedded_git_supports_tty_prompts_resizing_and_input() {
    let f = Fixture::new();
    f.hook("#!/bin/sh\n[ -n \"$GPG_TTY\" ] || exit 7\nprintf 'PUSH_INPUT: ' > \"$GPG_TTY\"\nread answer < /dev/tty\n[ \"$answer\" = yes ]\n");
    let mut job = f.start(Action::Push);
    job.resize(20, 90).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("PUSH_INPUT:") {
        assert!(
            Instant::now() < deadline,
            "Missing prompt: {}",
            String::from_utf8_lossy(&output)
        );
        match job.poll() {
            Some(Event::Output(bytes)) => output.extend(bytes),
            Some(Event::Finished(result)) => panic!(
                "Git finished before input: {result:?}; {}",
                String::from_utf8_lossy(&output)
            ),
            Some(Event::InputError(error)) => panic!("{error}"),
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    job.write(b"yes\r").unwrap();
    finish(&mut job).1.unwrap();
}

#[cfg(unix)]
#[test]
fn interrupting_embedded_git_reports_failure_and_reaps_process() {
    let f = Fixture::new();
    f.hook("#!/bin/sh\necho WAITING_FOR_INPUT\nread answer < /dev/tty\n");
    let mut job = f.start(Action::Push);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("WAITING_FOR_INPUT") {
        assert!(Instant::now() < deadline);
        match job.poll() {
            Some(Event::Output(bytes)) => output.extend(bytes),
            Some(Event::Finished(result)) => panic!("Unexpected completion: {result:?}"),
            Some(Event::InputError(error)) => panic!("{error}"),
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    job.write(b"\x03").unwrap();
    assert!(finish(&mut job).1.is_err());
}
