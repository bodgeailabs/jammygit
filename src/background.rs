//! Repository IO never runs on the terminal's input/render thread.
use crate::{
    git::{Action, Entry, Plan, Repo, Result, Snapshot},
    network::Job,
};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

struct Mailbox<I> {
    pending: Option<I>,
    stopped: bool,
}
pub struct Latest<I, O> {
    mailbox: Arc<(Mutex<Mailbox<I>>, Condvar)>,
    results: Receiver<O>,
}
impl<I: Send + 'static, O: Send + 'static> Latest<I, O> {
    pub fn new(mut run: impl FnMut(I) -> O + Send + 'static) -> Self {
        let mailbox = Arc::new((
            Mutex::new(Mailbox {
                pending: None,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let shared = Arc::clone(&mailbox);
        let (sender, results) = mpsc::sync_channel(2);
        thread::spawn(move || {
            loop {
                let (lock, ready) = &*shared;
                let mut state = lock.lock().unwrap();
                while state.pending.is_none() && !state.stopped {
                    state = ready.wait(state).unwrap();
                }
                if state.stopped {
                    break;
                }
                let request = state.pending.take().unwrap();
                drop(state);
                if sender.send(run(request)).is_err() {
                    break;
                }
            }
        });
        Self { mailbox, results }
    }
    pub fn request(&self, request: I) {
        let (lock, ready) = &*self.mailbox;
        lock.lock().unwrap().pending = Some(request);
        ready.notify_one();
    }
    pub fn poll(&self) -> Option<O> {
        self.results.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn queued_reads_keep_only_the_latest_request() {
        let (started, reading) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = Latest::new(move |value| {
            started.send(value).unwrap();
            if value == 1 {
                gate.recv().unwrap();
            }
            value
        });
        worker.request(1);
        assert_eq!(reading.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        worker.request(2);
        worker.request(3);
        release.send(()).unwrap();
        assert_eq!(reading.recv_timeout(Duration::from_secs(2)).unwrap(), 3);
    }
}
impl<I, O> Drop for Latest<I, O> {
    fn drop(&mut self) {
        let (lock, ready) = &*self.mailbox;
        lock.lock().unwrap().stopped = true;
        ready.notify_one();
    }
}

pub struct View {
    pub state: Snapshot,
    pub log: String,
}
pub fn read_view(repo: &Repo) -> Result<View> {
    let state = repo.snapshot()?;
    let log = repo
        .log_for(&state)
        .unwrap_or_else(|error| format!("Cannot read history: {error}"));
    Ok(View { state, log })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffKey {
    pub entry: Option<Entry>,
    pub staged: bool,
}
impl DiffKey {
    /// File status can change while refreshing the same visible patch.
    pub fn same_view(&self, other: &Self) -> bool {
        self.staged == other.staged
            && self.entry.as_ref().map(|entry| &entry.path)
                == other.entry.as_ref().map(|entry| &entry.path)
    }
}
pub struct DiffRequest {
    pub id: u64,
    pub key: DiffKey,
}
pub enum Work {
    Review(u64),
    Commit { id: u64, message: String },
    Confirm(Action),
    Stage(Action),
    ToggleAllStaging,
    StagePlan(Plan),
    StartAction(Action),
    StartPlan(Plan),
}
pub enum Reply {
    Review(u64, Result<Plan>),
    Confirm(Result<Plan>),
    Staged(Result<View>),
    Started(Result<(Plan, Job)>),
}

pub struct Background {
    pub refresh: Latest<u64, (u64, Result<View>)>,
    pub diff: Latest<DiffRequest, (u64, Result<String>)>,
    sender: Sender<Work>,
    pub results: Receiver<Reply>,
}
impl Background {
    pub fn new(repo: &Repo) -> Self {
        let refresh_repo = repo.clone();
        let refresh = Latest::new(move |id| (id, read_view(&refresh_repo)));
        let diff_repo = repo.clone();
        let diff = Latest::new(move |request: DiffRequest| {
            (
                request.id,
                diff_repo.diff(request.key.entry.as_ref(), request.key.staged),
            )
        });
        let repo = repo.clone();
        let (sender, receiver) = mpsc::channel();
        let (replies, results) = mpsc::channel();
        thread::spawn(move || {
            let mut review: Option<(u64, Result<Plan>)> = None;
            let start = |plan: Plan| Job::start(&repo, &plan, 24, 80).map(|job| (plan, job));
            while let Ok(work) = receiver.recv() {
                let reply = match work {
                    Work::Review(id) => {
                        let plan = repo.plan(Action::Commit("review".into()));
                        review = Some((id, plan.clone()));
                        Reply::Review(id, plan)
                    }
                    Work::Commit { id, message } => {
                        let result = review
                            .as_ref()
                            .filter(|(current, _)| *current == id)
                            .ok_or_else(|| {
                                "Commit review expired. Press c to reopen the composer.".to_string()
                            })
                            .and_then(|(_, plan)| plan.clone())
                            .and_then(|plan| plan.with_commit_message(message))
                            .and_then(start);
                        Reply::Started(result)
                    }
                    Work::Confirm(action) => Reply::Confirm(repo.plan(action)),
                    Work::Stage(action) => Reply::Staged(
                        repo.plan(action)
                            .and_then(|plan| repo.execute_staging(&plan))
                            .and_then(|_| read_view(&repo)),
                    ),
                    Work::StagePlan(plan) => {
                        Reply::Staged(repo.execute_staging(&plan).and_then(|_| read_view(&repo)))
                    }
                    Work::ToggleAllStaging => {
                        Reply::Staged(repo.toggle_all_staging().and_then(|_| read_view(&repo)))
                    }
                    Work::StartAction(action) => Reply::Started(repo.plan(action).and_then(start)),
                    Work::StartPlan(plan) => Reply::Started(start(plan)),
                };
                if replies.send(reply).is_err() {
                    break;
                }
            }
        });
        Self {
            refresh,
            diff,
            sender,
            results,
        }
    }
    pub fn send(&self, work: Work) -> Result<()> {
        self.sender
            .send(work)
            .map_err(|_| "The Git worker stopped unexpectedly.".into())
    }
}
