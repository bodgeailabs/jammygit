use std::{
    collections::VecDeque,
    env,
    io::{self, IsTerminal},
    num::NonZeroU16,
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute,
};
use jammygit::background::{Background, DiffKey, DiffRequest, Reply, View, Work};
use jammygit::git::{Action, Plan, Repo, Snapshot, safe_text};
use jammygit::network::{Event as NetworkEvent, Job};
use ratatui::{
    layout::{Position, Rect},
    widgets::ListState,
};

mod history;
mod ui;

#[derive(Default)]
struct Editor {
    text: String,
    cursor: usize,
}

impl Editor {
    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert('\n')
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => self.insert('\n'),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(c)
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home => {
                self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |n| n + 1)
            }
            KeyCode::End => {
                self.cursor = self.text[self.cursor..]
                    .find('\n')
                    .map_or(self.text.len(), |n| self.cursor + n)
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.previous();
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.drain(self.cursor..self.next());
            }
            _ => {}
        }
    }

    fn insert(&mut self, c: char) {
        if c == '\n' || !c.is_control() {
            self.text.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }
}

enum Modal {
    Commit,
    Confirm(Box<Plan>),
    Help,
    History,
    Error(String),
}

struct App {
    repo: Repo,
    state: Option<Snapshot>,
    selection: ListState,
    staged: bool,
    all_diff: bool,
    diff: String,
    log: String,
    scroll: u16,
    scroll_x: u16,
    drawer_open: bool,
    drawer_progress: f32,
    drawer_from: f32,
    drawer_changed: Instant,
    notice: String,
    failed: bool,
    modal: Option<Modal>,
    editor: Editor,
    commit_review: Option<Plan>,
    refreshed: Instant,
    network: Option<Job>,
    network_output: Option<vt100::Parser>,
    network_title: String,
    job_is_commit: bool,
    network_input: bool,
    show_output: bool,
    output_size: (u16, u16),
    background: Background,
    refresh_id: u64,
    refresh_pending: bool,
    diff_id: u64,
    diff_key: Option<DiffKey>,
    displayed_diff_key: Option<DiffKey>,
    diff_pending: bool,
    diff_due: Option<(Instant, DiffKey)>,
    diff_cache: VecDeque<(DiffKey, String)>,
    pending_actions: usize,
    staging_pending: bool,
    review_id: u64,
    review_pending: bool,
    transcript: history::Transcript,
    active_turn: Option<u64>,
    history_area: Rect,
    diff_area: Rect,
}

impl App {
    fn new(repo: Repo) -> Self {
        let mut app = Self {
            background: Background::new(&repo),
            repo,
            state: None,
            selection: ListState::default(),
            staged: false,
            all_diff: false,
            diff: String::new(),
            log: String::new(),
            scroll: 0,
            scroll_x: 0,
            drawer_open: false,
            drawer_progress: 0.0,
            drawer_from: 0.0,
            drawer_changed: Instant::now(),
            notice: "Loading repository…".into(),
            failed: false,
            modal: None,
            editor: Editor::default(),
            commit_review: None,
            refreshed: Instant::now(),
            network: None,
            network_output: None,
            network_title: String::new(),
            job_is_commit: false,
            network_input: false,
            show_output: false,
            output_size: (24, 80),
            refresh_id: 0,
            refresh_pending: false,
            diff_id: 0,
            diff_key: None,
            displayed_diff_key: None,
            diff_pending: false,
            diff_due: None,
            diff_cache: VecDeque::new(),
            pending_actions: 0,
            staging_pending: false,
            review_id: 0,
            review_pending: false,
            transcript: history::Transcript::default(),
            active_turn: None,
            history_area: Rect::default(),
            diff_area: Rect::default(),
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.refresh_id += 1;
        self.refresh_pending = true;
        self.refreshed = Instant::now();
        self.background.refresh.request(self.refresh_id);
    }

    fn apply_view(&mut self, view: View) {
        let selected = self
            .state
            .as_ref()
            .and_then(|s| self.selection.selected().and_then(|i| s.entries.get(i)))
            .map(|e| e.path.clone());
        let index = selected
            .and_then(|path| view.state.entries.iter().position(|e| e.path == path))
            .or(self.selection.selected())
            .unwrap_or(0);
        self.selection.select(if view.state.entries.is_empty() {
            None
        } else {
            Some(index.min(view.state.entries.len() - 1))
        });
        self.state = Some(view.state);
        self.log = view.log;
        if self.notice == "Loading repository…" {
            self.notice = "↑/↓ selects a file. Enter opens its diff.".into();
        }
        self.diff_cache.clear();
        self.diff_id += 1;
        self.diff_key = None;
        self.diff_pending = false;
        self.diff_due = None;
        self.update_diff();
        self.refreshed = Instant::now();
    }

    fn update_diff(&mut self) {
        if !self.drawer_open {
            return;
        }
        let entry = if self.all_diff {
            None
        } else {
            self.state
                .as_ref()
                .and_then(|s| self.selection.selected().and_then(|i| s.entries.get(i)))
                .cloned()
        };
        if entry.is_none() && !self.all_diff {
            self.diff = "No file selected.".into();
            self.diff_id += 1;
            self.diff_key = None;
            self.displayed_diff_key = None;
            self.diff_pending = false;
            self.diff_due = None;
            return;
        }
        let key = DiffKey {
            entry,
            staged: self.staged,
        };
        if self.diff_key.as_ref() == Some(&key) {
            return;
        }
        self.diff_id += 1;
        self.diff_key = Some(key.clone());
        self.diff_pending = false;
        self.diff_due = None;
        if key.entry.as_ref().is_some_and(|e| e.index == b'?') {
            self.diff =
                "Untracked file. Space stages it for review; c commits staged changes only.".into();
            self.displayed_diff_key = Some(key);
        } else if let Some((_, text)) = self.diff_cache.iter().find(|(cached, _)| *cached == key) {
            self.diff = text.clone();
            self.displayed_diff_key = Some(key);
        } else {
            if !self
                .displayed_diff_key
                .as_ref()
                .is_some_and(|displayed| displayed.same_view(&key))
            {
                self.diff = "Loading diff…".into();
                self.displayed_diff_key = None;
            }
            self.diff_pending = true;
            self.diff_due = Some((Instant::now() + Duration::from_millis(25), key));
        }
    }

    fn submit(&mut self, work: Work) {
        self.pending_actions += 1;
        self.refresh_id += 1;
        self.refresh_pending = false;
        if let Err(error) = self.background.send(work) {
            self.pending_actions -= 1;
            self.finish_turn(false, vec![safe_text(&error)]);
            self.error(error);
        }
    }

    fn poll_background(&mut self) -> bool {
        let mut changed = false;
        while let Some((id, result)) = self.background.refresh.poll() {
            if id != self.refresh_id {
                continue;
            }
            self.refresh_pending = false;
            if self.pending_actions > 0 || self.network.is_some() {
                continue;
            }
            changed = true;
            match result {
                Ok(view) => self.apply_view(view),
                Err(error) => {
                    self.state = None;
                    self.diff.clear();
                    self.diff_id += 1;
                    self.diff_key = None;
                    self.displayed_diff_key = None;
                    self.diff_pending = false;
                    self.diff_due = None;
                    self.log.clear();
                    self.notice = format!("Refresh failed: {error}. Press r to retry.");
                    self.failed = true;
                }
            }
        }
        while let Some((id, result)) = self.background.diff.poll() {
            if id != self.diff_id || !self.drawer_open {
                continue;
            }
            let text = result.unwrap_or_else(|error| error);
            changed |= self.diff != text;
            self.diff = text;
            self.diff_pending = false;
            self.displayed_diff_key = self.diff_key.clone();
            if let Some(key) = &self.diff_key {
                self.diff_cache.push_back((key.clone(), self.diff.clone()));
                while self.diff_cache.len() > 8 {
                    self.diff_cache.pop_front();
                }
            }
        }
        if self
            .diff_due
            .as_ref()
            .is_some_and(|(due, _)| Instant::now() >= *due)
        {
            let (_, key) = self.diff_due.take().unwrap();
            if self.drawer_open {
                self.background.diff.request(DiffRequest {
                    id: self.diff_id,
                    key,
                });
            }
        }
        for _ in 0..32 {
            let Ok(reply) = self.background.results.try_recv() else {
                break;
            };
            changed = true;
            match reply {
                Reply::Review(id, result) => {
                    if id != self.review_id {
                        continue;
                    }
                    self.review_pending = false;
                    match result {
                        Ok(plan) => self.commit_review = Some(plan),
                        Err(error) if matches!(self.modal, Some(Modal::Commit)) => {
                            self.error(error)
                        }
                        Err(_) => {}
                    }
                }
                Reply::Confirm(result) => {
                    self.pending_actions = self.pending_actions.saturating_sub(1);
                    match result {
                        Ok(plan) => self.modal = Some(Modal::Confirm(Box::new(plan))),
                        Err(error) => self.error(error),
                    }
                }
                Reply::Staged(result) => {
                    self.pending_actions = self.pending_actions.saturating_sub(1);
                    self.staging_pending = false;
                    match result {
                        Ok(view) => {
                            self.failed = false;
                            self.notice = "Staging updated. c writes a commit; Enter opens the selected diff.".into();
                            self.apply_view(view);
                        }
                        Err(error) => self.error(error),
                    }
                }
                Reply::Started(result) => {
                    self.pending_actions = self.pending_actions.saturating_sub(1);
                    match result {
                        Ok((plan, job)) => self.install_network(plan, job),
                        Err(error) => {
                            self.finish_turn(false, vec![safe_text(&error)]);
                            self.error(error);
                        }
                    }
                }
            }
        }
        changed
    }

    fn error(&mut self, error: String) {
        self.notice = error.clone();
        self.failed = true;
        self.modal = Some(Modal::Error(error));
    }

    fn stage(&mut self, action: Action) {
        self.notice = "Updating staging…".into();
        self.staging_pending = true;
        self.submit(Work::Stage(action));
    }

    fn key(&mut self, key: KeyEvent) -> Option<Plan> {
        if self.network.is_some() {
            self.network_key(key);
            return None;
        }
        if self.modal.is_none()
            && self.pending_actions > 0
            && (matches!(key.code, KeyCode::Char('P' | 'p' | 'a' | 'u' | ' '))
                || (key.code == KeyCode::Char('c') && !self.staging_pending))
        {
            return None;
        }
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Commit => {
                    if key.code == KeyCode::Esc {
                        return None;
                    }
                    if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::ALT) {
                        if self.editor.text.trim().is_empty() {
                            self.notice = "Enter a commit message first.".into();
                            self.failed = true;
                        } else {
                            self.notice = "Preparing commit…".into();
                            self.job_is_commit = true;
                            self.begin_turn(&Action::Commit(self.editor.text.clone()));
                            self.submit(Work::Commit {
                                id: self.review_id,
                                message: self.editor.text.clone(),
                            });
                            return None;
                        }
                    } else {
                        self.editor.key(key);
                    }
                    self.modal = Some(Modal::Commit);
                }
                Modal::Confirm(plan) => {
                    if matches!(key.code, KeyCode::Enter | KeyCode::Char('y')) {
                        if matches!(plan.action, Action::StageAll) {
                            self.staging_pending = true;
                            self.submit(Work::StagePlan(*plan));
                        } else {
                            return Some(*plan);
                        }
                    } else if !matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) {
                        self.modal = Some(Modal::Confirm(plan));
                    }
                }
                Modal::Help | Modal::History | Modal::Error(_) => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('g') => self.modal = Some(Modal::History),
            KeyCode::Enter | KeyCode::Right => {
                self.set_drawer(true);
                self.select_diff_view();
            }
            KeyCode::Esc | KeyCode::Left => self.set_drawer(false),
            KeyCode::Char('h') => self.scroll_x = self.scroll_x.saturating_sub(8),
            KeyCode::Char('l') => self.scroll_x = self.scroll_x.saturating_add(8),
            KeyCode::Char('r') => {
                self.failed = false;
                self.notice = "Refreshed from Git.".into();
                self.refresh();
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Tab => {
                self.staged = !self.staged;
                self.scroll = 0;
                self.set_drawer(true);
                self.update_diff();
            }
            KeyCode::Char('v') => {
                self.all_diff = !self.all_diff;
                self.scroll = 0;
                self.set_drawer(true);
                self.update_diff();
            }
            KeyCode::Char('o') => {
                self.show_output = !self.transcript.turns.is_empty() && !self.show_output;
                if self.show_output {
                    self.set_drawer(false);
                }
            }
            KeyCode::PageDown | KeyCode::Char('J') => self.scroll_output(false),
            KeyCode::PageUp | KeyCode::Char('K') => self.scroll_output(true),
            KeyCode::Home if !self.drawer_open => {
                self.show_output = !self.transcript.turns.is_empty();
                self.transcript.oldest();
            }
            KeyCode::End if !self.drawer_open => {
                self.show_output = !self.transcript.turns.is_empty();
                self.transcript.follow_latest();
            }
            KeyCode::Char(' ') | KeyCode::Char('u') => {
                if let Some(entry) = self
                    .state
                    .as_ref()
                    .and_then(|s| self.selection.selected().and_then(|i| s.entries.get(i)))
                    .cloned()
                {
                    let action = if key.code == KeyCode::Char('u') || !entry.unstaged() {
                        Action::Unstage(entry)
                    } else {
                        Action::Stage(entry)
                    };
                    self.stage(action);
                }
            }
            KeyCode::Char('c') => {
                if !self.staging_pending
                    && !self
                        .state
                        .as_ref()
                        .is_some_and(|s| s.entries.iter().any(|e| e.staged()))
                {
                    self.error(
                        "Nothing staged. Space stages the selected file; a stages all.".into(),
                    );
                } else {
                    self.staged = true;
                    self.all_diff = true;
                    self.set_drawer(false);
                    self.commit_review = None;
                    self.review_id += 1;
                    self.review_pending = true;
                    if let Err(error) = self.background.send(Work::Review(self.review_id)) {
                        self.error(error);
                    } else {
                        self.modal = Some(Modal::Commit);
                    }
                }
            }
            KeyCode::Char('a') => {
                self.notice = "Updating staging…".into();
                self.staging_pending = true;
                self.submit(Work::ToggleAllStaging);
            }
            KeyCode::Char('P') | KeyCode::Char('p') => {
                let action = if key.code == KeyCode::Char('P') {
                    Action::Push
                } else {
                    Action::Pull
                };
                self.notice = "Preparing Git operation…".into();
                self.job_is_commit = false;
                self.begin_turn(&action);
                self.submit(Work::StartAction(action));
            }
            _ => {}
        }
        None
    }

    fn start_network(&mut self, plan: Plan) {
        if self.active_turn.is_none() {
            self.begin_turn(&plan.action);
        }
        self.submit(Work::StartPlan(plan));
    }

    fn begin_turn(&mut self, action: &Action) {
        let (title, request) = match action {
            Action::Commit(message) => ("Commit staged changes", message.clone()),
            Action::Push => ("Push branch", "Push the current branch.".into()),
            Action::Pull => (
                "Pull updates",
                "Pull updates for the current branch.".into(),
            ),
            _ => return,
        };
        self.active_turn = Some(
            self.transcript.begin(
                title.into(),
                request
                    .lines()
                    .map(safe_text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        );
        self.show_output = true;
    }

    fn finish_turn(&mut self, success: bool, output: Vec<String>) {
        if let Some(id) = self.active_turn.take() {
            self.transcript.finish(id, success, output);
        }
    }

    fn install_network(&mut self, plan: Plan, job: Job) {
        if self.active_turn.is_none() {
            self.begin_turn(&plan.action);
        }
        if let Some(id) = self.active_turn {
            self.transcript.running(id, plan.title.clone());
        }
        let (rows, cols) = self.output_size;
        let mut parser = vt100::Parser::new(nonzero(rows), nonzero(cols), 1000);
        let detail = plan
            .detail
            .lines()
            .map(safe_text)
            .collect::<Vec<_>>()
            .join("\r\n");
        parser.process(format!("{detail}\r\n\r\n").as_bytes());
        self.network_output = Some(parser);
        self.job_is_commit = matches!(plan.action, Action::Commit(_));
        self.network_title = plan.title;
        self.network = Some(job);
        self.set_drawer(false);
        self.network_input = false;
        self.show_output = true;
        self.failed = false;
        self.notice = format!(
            "{} running · i type into Git · Ctrl+C interrupt",
            self.network_title
        );
    }

    fn poll_network(&mut self) -> bool {
        let mut changed = false;
        // Keep drawing and accepting input even when Git produces lots of output.
        for _ in 0..64 {
            let Some(event) = self.network.as_mut().and_then(Job::poll) else {
                break;
            };
            changed = true;
            match event {
                NetworkEvent::InputError(error) => {
                    self.notice = error;
                    self.failed = true;
                }
                NetworkEvent::Output(bytes) => {
                    if let Some(parser) = &mut self.network_output {
                        parser.process(&bytes);
                    }
                }
                NetworkEvent::Finished(result) => {
                    let message = match &result {
                        Ok(()) => format!("{} succeeded.", self.network_title),
                        Err(error) => error.clone(),
                    };
                    if let Some(parser) = &mut self.network_output {
                        parser.screen_mut().set_scrollback(0);
                        parser.process(format!("\r\n{}\r\n", safe_text(&message)).as_bytes());
                    }
                    let output = if let Some(parser) = &mut self.network_output {
                        let height = history::terminal_height(parser);
                        let mut lines = history::terminal_rows(parser, 0, height);
                        while lines.last().is_some_and(|line| line.trim().is_empty()) {
                            lines.pop();
                        }
                        lines
                    } else {
                        vec![message.clone()]
                    };
                    self.finish_turn(result.is_ok(), output);
                    self.network_output = None;
                    self.network = None;
                    self.network_input = false;
                    self.failed = result.is_err();
                    self.notice = if self.job_is_commit && result.is_err() {
                        format!("{message} Draft saved; c to retry · o output")
                    } else {
                        format!("{message}  PgUp scrolls history · End returns to latest")
                    };
                    if self.job_is_commit && result.is_ok() {
                        self.editor = Editor::default();
                        self.commit_review = None;
                    }
                    self.refresh();
                    break;
                }
            }
        }
        changed
    }

    fn network_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.network_input = false;
            self.notice = format!(
                "{} running · i type into Git · Ctrl+C interrupt",
                self.network_title
            );
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.send_network(b"\x03");
            self.notice = "Interrupt sent to Git; waiting for it to finish.".into();
            return;
        }
        if self.network_input {
            if let Some(bytes) = terminal_key(key) {
                self.transcript.follow_latest();
                self.send_network(&bytes);
            }
        } else {
            match key.code {
                KeyCode::Char('i') => {
                    self.network_input = true;
                    self.transcript.follow_latest();
                    if let Some(parser) = &mut self.network_output {
                        parser.screen_mut().set_scrollback(0);
                    }
                    self.notice =
                        "Typing into Git · Esc returns to navigation · Ctrl+C interrupts Git"
                            .into();
                }
                KeyCode::PageUp | KeyCode::Char('K') => self.scroll_output(true),
                KeyCode::PageDown | KeyCode::Char('J') => self.scroll_output(false),
                KeyCode::Home => self.transcript.oldest(),
                KeyCode::End => self.transcript.follow_latest(),
                _ => {}
            }
        }
    }

    fn send_network(&mut self, bytes: &[u8]) {
        if let Some(job) = &mut self.network
            && let Err(error) = job.write(bytes)
        {
            self.notice = error;
            self.failed = true;
        }
    }

    fn scroll_output(&mut self, up: bool) {
        if self.drawer_open {
            self.scroll = if up {
                self.scroll.saturating_sub(10)
            } else {
                self.scroll.saturating_add(10)
            };
        } else {
            self.show_output = !self.transcript.turns.is_empty();
            self.transcript.scroll(up, 10);
        }
    }

    fn mouse(&mut self, event: MouseEvent) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let up = match event.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return false,
        };
        if self.drawer_open
            && self
                .diff_area
                .contains(Position::new(event.column, event.row))
        {
            self.scroll = if up {
                self.scroll.saturating_sub(3)
            } else {
                self.scroll.saturating_add(3)
            };
        } else if self
            .history_area
            .contains(Position::new(event.column, event.row))
        {
            self.show_output = !self.transcript.turns.is_empty();
            self.transcript.scroll(up, 3);
        } else {
            return false;
        }
        true
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.state.as_ref().map_or(0, |s| s.entries.len());
        if len > 0 {
            let next = self
                .selection
                .selected()
                .unwrap_or(0)
                .saturating_add_signed(delta)
                .min(len - 1);
            if self.selection.selected() == Some(next) {
                return;
            }
            self.selection.select(Some(next));
            self.scroll = 0;
            self.scroll_x = 0;
            if self.drawer_open {
                self.select_diff_view();
            } else {
                self.update_diff();
            }
        }
    }

    fn set_drawer(&mut self, open: bool) {
        if self.drawer_open != open {
            self.drawer_from = self.drawer_progress;
            self.drawer_changed = Instant::now();
            self.drawer_open = open;
            if !open {
                self.diff_id += 1;
                self.diff_key = None;
                self.diff_pending = false;
                self.diff_due = None;
            }
        }
    }

    fn select_diff_view(&mut self) {
        self.all_diff = false;
        if let Some(entry) = self
            .state
            .as_ref()
            .and_then(|s| self.selection.selected().and_then(|i| s.entries.get(i)))
        {
            self.staged = entry.staged() && !entry.unstaged();
        }
        self.scroll = 0;
        self.scroll_x = 0;
        self.update_diff();
    }
}

fn nonzero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value.max(1)).unwrap()
}

fn terminal_key(key: KeyEvent) -> Option<Vec<u8>> {
    let mut bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii() => {
            vec![(c.to_ascii_uppercase() as u8) & 0x1f]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

// Drop restores the terminal on ordinary errors and unwinding. Ratatui also installs a panic hook.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        ratatui::restore();
    }
}

fn main() {
    if let Err(error) = start() {
        eprintln!("jammygit: {error}");
        std::process::exit(1);
    }
}

fn start() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|v| v == "--help" || v == "-h") {
        println!(
            "jammygit — a small, careful Git TUI\n\nUsage: jammygit [repository-path]\n\nSpace stage · c commit staged · P push · p pull · ? help · q quit\nRequires Git on PATH. Uses your existing Git credentials, hooks and signing."
        );
        return Ok(());
    }
    if args.first().is_some_and(|v| v == "--version" || v == "-V") {
        println!("jammygit {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.len() > 1 {
        return Err("Usage: jammygit [repository-path]".into());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("Run jammygit in an interactive terminal. See --help for usage.".into());
    }
    let path = args
        .first()
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    let repo = Repo::discover(&path).map_err(io::Error::other)?;
    let mut app = App::new(repo);
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    let mut dirty = true;
    'running: loop {
        dirty |= app.poll_background();
        dirty |= app.poll_network();
        let animating =
            (app.drawer_progress - if app.drawer_open { 1.0 } else { 0.0 }).abs() > 0.001;
        if dirty || animating {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
        // Polling wakes immediately on input; completed worker results wait at most one frame.
        // Unchanged idle screens are not redrawn.
        if event::poll(Duration::from_millis(16))? {
            for _ in 0..64 {
                match event::read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        let quit = (key.code == KeyCode::Char('q') && app.modal.is_none())
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL));
                        if app.network.is_none() && quit {
                            if app.pending_actions == 0 {
                                break 'running;
                            }
                            app.notice = "Git is preparing an operation; wait for it to start before interrupting.".into();
                        } else if let Some(plan) = app.key(key) {
                            app.start_network(plan);
                        }
                        dirty = true;
                    }
                    Event::Paste(text) if matches!(app.modal, Some(Modal::Commit)) => {
                        for c in text.chars() {
                            app.editor.insert(c);
                        }
                        dirty = true;
                    }
                    Event::Paste(text) if app.network_input => {
                        let bracketed = app
                            .network_output
                            .as_ref()
                            .is_some_and(|p| p.screen().bracketed_paste());
                        let text = if bracketed {
                            format!("\x1b[200~{text}\x1b[201~")
                        } else {
                            text
                        };
                        app.send_network(text.as_bytes());
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    Event::Mouse(mouse) => dirty |= app.mouse(mouse),
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if app.modal.is_none()
            && app.network.is_none()
            && app.pending_actions == 0
            && !app.refresh_pending
            && app.refreshed.elapsed() >= Duration::from_secs(3)
        {
            app.refresh();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    #[ignore = "prints interaction timings for manual performance comparison"]
    fn interaction_benchmark() {
        let (_dir, mut app) = test_app();
        app.state.as_mut().unwrap().entries = (0..5000)
            .map(|i| jammygit::git::Entry {
                index: b'M',
                worktree: b' ',
                path: format!("file-{i:05}.rs").into(),
                original: None,
            })
            .collect();
        app.selection.select(Some(0));
        let percentile = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            values[(values.len() * 95 / 100).min(values.len() - 1)]
        };
        for drawer in [false, true] {
            app.drawer_open = drawer;
            let mut samples = Vec::new();
            for _ in 0..30 {
                let started = Instant::now();
                app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "navigation_drawer_{drawer}_p95_ms={:.3}",
                percentile(samples)
            );
        }
        app.drawer_open = false;
        app.drawer_progress = 0.0;
        app.drawer_from = 0.0;
        let mut terminal = Terminal::new(TestBackend::new(120, 38)).unwrap();
        let mut samples = Vec::new();
        for _ in 0..20 {
            let started = Instant::now();
            terminal.draw(|f| app.draw(f)).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        println!("render_5000_files_p95_ms={:.3}", percentile(samples));
        for index in 0..100 {
            app.begin_turn(&Action::Commit(format!("feat: action {index}")));
            app.finish_turn(
                true,
                (0..80).map(|line| format!("Output line {line}")).collect(),
            );
        }
        terminal.draw(|f| app.draw(f)).unwrap();
        let mut samples = Vec::new();
        for _ in 0..20 {
            let started = Instant::now();
            app.scroll_output(true);
            terminal.draw(|f| app.draw(f)).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        println!("render_100_actions_p95_ms={:.3}", percentile(samples));
        let started = Instant::now();
        app.repo.snapshot().unwrap();
        println!(
            "snapshot_ms={:.3}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    fn test_app() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["init", "--initial-branch=main"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let repo = Repo::discover(dir.path()).unwrap();
        let mut app = App::new(repo);
        settle(&mut app);
        (dir, app)
    }

    fn settle(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll_background();
            app.poll_network();
            if app.pending_actions == 0
                && app.network.is_none()
                && !app.refresh_pending
                && !app.review_pending
                && app.diff_due.is_none()
                && !app.diff_pending
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Workers did not settle: {}",
                app.notice
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn stale_diff_results_cannot_replace_the_newly_selected_file() {
        use jammygit::background::Latest;
        use std::sync::mpsc;
        let (_dir, mut app) = test_app();
        app.state.as_mut().unwrap().entries = ["first", "second"]
            .into_iter()
            .map(|path| jammygit::git::Entry {
                index: b'M',
                worktree: b' ',
                path: path.into(),
                original: None,
            })
            .collect();
        app.selection.select(Some(0));
        let (started, reading) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        app.background.diff = Latest::new(move |request: DiffRequest| {
            let path = request.key.entry.unwrap().path;
            started.send(path.clone()).unwrap();
            gate.recv().unwrap();
            (request.id, Ok(path.to_string_lossy().into_owned()))
        });
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.diff_due.as_mut().unwrap().0 = Instant::now();
        app.poll_background();
        assert_eq!(
            reading.recv_timeout(Duration::from_secs(2)).unwrap(),
            PathBuf::from("first")
        );
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.diff_due.as_mut().unwrap().0 = Instant::now();
        app.poll_background();
        release.send(()).unwrap();
        assert_eq!(
            reading.recv_timeout(Duration::from_secs(2)).unwrap(),
            PathBuf::from("second")
        );
        app.poll_background();
        assert_eq!(app.diff, "Loading diff…");
        release.send(()).unwrap();
        settle(&mut app);
        assert_eq!(app.diff, "second");
    }

    #[test]
    fn refreshing_file_and_staged_diffs_keeps_the_visible_frame_until_ready() {
        use jammygit::background::Latest;
        use std::sync::mpsc;
        const ORIGINAL: &str =
            "@@ -1,4 +1,4 @@\n context\n-old value\n+original value\n more context\n last line";
        const UPDATED: &str =
            "@@ -1,4 +1,4 @@\n context\n-old value\n+updated value\n more context\n last line";
        for (all_diff, staged) in [(false, false), (false, true), (true, true)] {
            for replacement in [ORIGINAL, UPDATED] {
                let (_dir, mut app) = test_app();
                app.state.as_mut().unwrap().entries = vec![jammygit::git::Entry {
                    index: b'M',
                    worktree: b'M',
                    path: "file.rs".into(),
                    original: None,
                }];
                app.selection.select(Some(0));
                app.drawer_open = true;
                app.drawer_progress = 1.0;
                app.drawer_from = 1.0;
                app.all_diff = all_diff;
                app.staged = staged;
                let (started, reading) = mpsc::channel();
                let (release, gate) = mpsc::channel();
                let mut calls = 0;
                app.background.diff = Latest::new(move |request: DiffRequest| {
                    calls += 1;
                    let text = if calls == 1 {
                        ORIGINAL
                    } else {
                        started.send(()).unwrap();
                        gate.recv().unwrap();
                        replacement
                    };
                    (request.id, Ok(text.into()))
                });
                app.update_diff();
                settle(&mut app);
                app.scroll = 3;
                app.scroll_x = 2;
                let mut terminal = Terminal::new(TestBackend::new(120, 38)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let before = terminal.backend().buffer().clone();
                app.apply_view(View {
                    state: app.state.clone().unwrap(),
                    log: app.log.clone(),
                });
                assert_eq!(app.diff, ORIGINAL);
                assert_eq!((app.scroll, app.scroll_x), (3, 2));
                terminal.draw(|frame| app.draw(frame)).unwrap();
                assert_eq!(
                    terminal.backend().buffer(),
                    &before,
                    "refresh must not flash a loading frame"
                );
                app.diff_due.as_mut().unwrap().0 = Instant::now();
                app.poll_background();
                reading.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(app.diff, ORIGINAL);
                release.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut repainted = false;
                while app.diff_pending {
                    assert!(Instant::now() < deadline);
                    repainted |= app.poll_background();
                    std::thread::sleep(Duration::from_millis(2));
                }
                assert_eq!(app.diff, replacement);
                assert_eq!(
                    repainted,
                    replacement != ORIGINAL,
                    "unchanged patches need no extra redraw"
                );
                assert_eq!((app.scroll, app.scroll_x), (3, 2));
            }
        }
    }

    #[test]
    fn navigation_with_closed_drawer_never_schedules_a_diff() {
        let (dir, mut app) = test_app();
        for name in ["first", "second"] {
            std::fs::write(dir.path().join(name), "content").unwrap();
        }
        app.refresh();
        settle(&mut app);
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.diff_due.is_none());
        assert!(app.diff_key.is_none());
    }

    #[test]
    fn push_and_pull_keys_return_operations_without_confirmation() {
        let (dir, mut app) = test_app();
        for args in [
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-m",
                "feat: initial",
            ],
            vec!["config", "branch.main.remote", "."],
            vec!["config", "branch.main.merge", "refs/heads/main"],
        ] {
            let output = std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for key in ['P', 'p'] {
            assert!(
                app.key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE))
                    .is_none()
            );
            assert_eq!(app.pending_actions, 1);
            assert!(app.modal.is_none());
            settle(&mut app);
            assert!(!matches!(app.modal, Some(Modal::Confirm(_))));
        }
        assert_eq!(app.transcript.turns.len(), 2);
        assert!(
            app.transcript
                .turns
                .iter()
                .all(|turn| turn.status == history::Status::Succeeded)
        );
        assert!(
            app.transcript
                .turns
                .front()
                .unwrap()
                .output
                .iter()
                .any(|line| line.contains("succeeded"))
        );
    }

    #[test]
    fn embedded_terminal_keys_preserve_control_and_unicode_input() {
        assert_eq!(
            terminal_key(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE)).unwrap(),
            "好".as_bytes()
        );
        assert_eq!(
            terminal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap(),
            b"\r"
        );
        assert_eq!(
            terminal_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)).unwrap(),
            b"\x04"
        );
    }

    #[test]
    fn arrows_select_files_enter_opens_drawer_and_escape_closes_it() {
        let (dir, mut app) = test_app();
        std::fs::write(dir.path().join("first.txt"), "one").unwrap();
        std::fs::write(dir.path().join("second.txt"), "two").unwrap();
        app.refresh();
        settle(&mut app);
        assert_eq!(app.selection.selected(), Some(0));
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selection.selected(), Some(1));
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.drawer_open);
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.selection.selected(), Some(0));
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.drawer_open);
    }

    #[cfg(unix)]
    #[test]
    fn embedded_commit_clears_successful_draft_and_keeps_rejected_draft() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, mut app) = test_app();
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/test-hooks"),
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(dir.path())
                    .args(["config", key, value])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        for rejected in [false, true] {
            if rejected {
                let hooks = dir.path().join(".git/test-hooks");
                std::fs::create_dir(&hooks).unwrap();
                let hook = hooks.join("pre-commit");
                std::fs::write(&hook, "#!/bin/sh\necho REJECTED\nexit 1\n").unwrap();
                std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::fs::write(
                dir.path().join("file"),
                if rejected { "second" } else { "first" },
            )
            .unwrap();
            app.stage(Action::StageAll);
            app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
            app.editor.text = "feat: retained draft".into();
            app.editor.cursor = app.editor.text.len();
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            settle(&mut app);
            assert_eq!(app.failed, rejected);
            assert_eq!(
                app.editor.text,
                if rejected { "feat: retained draft" } else { "" }
            );
            assert!(app.modal.is_none());
            let turn = app.transcript.turns.back().unwrap();
            assert_eq!(turn.request, "feat: retained draft");
            assert_eq!(
                turn.status,
                if rejected {
                    history::Status::Failed
                } else {
                    history::Status::Succeeded
                }
            );
            if rejected {
                assert!(turn.output.iter().any(|line| line.contains("REJECTED")));
                assert_eq!(app.transcript.turns.len(), 2);
            }
        }
    }

    #[test]
    fn a_toggles_all_files_immediately_using_current_git_state() {
        let (dir, mut app) = test_app();
        // The displayed snapshot is intentionally older than these files.
        for name in ["first", "second"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.modal.is_none());
        assert_eq!(app.pending_actions, 1);
        // Repeated input while staging must not queue a second toggle.
        app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.pending_actions, 1);
        settle(&mut app);
        assert!(!app.failed);
        assert!(
            app.state
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.staged() && !entry.unstaged())
        );
        app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.modal.is_none());
        settle(&mut app);
        assert!(!app.failed);
        assert!(
            app.state
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .all(|entry| !entry.staged())
        );
        for name in ["first", "second"] {
            assert_eq!(
                std::fs::read_to_string(dir.path().join(name)).unwrap(),
                name
            );
        }
    }

    #[test]
    fn stage_all_rejects_changes_while_confirmation_is_open() {
        let (dir, mut app) = test_app();
        std::fs::write(dir.path().join("first"), "first").unwrap();
        app.refresh();
        app.submit(Work::Confirm(Action::StageAll));
        settle(&mut app);
        std::fs::write(dir.path().join("surprise"), "new file after review").unwrap();
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        settle(&mut app);
        assert!(matches!(app.modal, Some(Modal::Error(_))));
        assert!(
            app.repo
                .snapshot()
                .unwrap()
                .entries
                .iter()
                .all(|e| !e.staged())
        );
    }

    #[test]
    fn commit_editor_rejects_changed_index_and_keeps_draft() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("file");
        std::fs::write(&path, "reviewed").unwrap();
        app.stage(Action::StageAll);
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        settle(&mut app);
        for c in "feat: saved draft".chars() {
            app.editor.insert(c);
        }
        std::fs::write(&path, "changed after review").unwrap();
        app.repo
            .execute_staging(&app.repo.plan(Action::StageAll).unwrap())
            .unwrap();
        assert!(
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .is_none()
        );
        settle(&mut app);
        assert!(matches!(app.modal, Some(Modal::Error(_))));
        assert_eq!(app.editor.text, "feat: saved draft");
        assert!(app.repo.snapshot().unwrap().head.is_none());
        assert_eq!(
            app.transcript.turns.back().unwrap().status,
            history::Status::Failed
        );
        assert!(app.active_turn.is_none());
    }

    #[test]
    fn session_timeline_scrolls_retains_history_and_renders_at_all_sizes() {
        use ratatui::{Terminal, backend::TestBackend};
        let (_dir, mut app) = test_app();
        for index in 0..3 {
            app.begin_turn(&Action::Commit(format!("feat: request {index}")));
            app.finish_turn(
                true,
                (0..20)
                    .map(|line| format!("Action {index} output {line}"))
                    .collect(),
            );
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 38)).unwrap();
        let text = |terminal: &Terminal<TestBackend>| {
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(text(&terminal).contains("Action 2 output 19"));
        app.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(text(&terminal).contains("feat: request 0"));
        assert!(!app.transcript.following());
        app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(text(&terminal).contains("Action 2 output 19"));
        app.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        assert!(!app.show_output);
        app.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(app.show_output);
        assert_eq!(app.transcript.turns.len(), 3);
        for (width, height) in [(64, 18), (80, 24), (120, 38)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            app.network_output = Some(vt100::Parser::new(nonzero(3), nonzero(30), 1000));
            app.begin_turn(&Action::Push);
            app.network_output
                .as_mut()
                .unwrap()
                .process(b"interactive prompt: ");
            app.network_input = true;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            app.finish_turn(false, vec!["Stopped".into()]);
            app.network_output = None;
            app.network_input = false;
        }
    }

    #[test]
    fn editor_preserves_unicode_boundaries() {
        let mut editor = Editor::default();
        for c in "feat: café 🦀".chars() {
            editor.insert(c);
        }
        editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        editor.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(editor.text, "feat: café ");
        editor.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        editor.insert('\n');
        editor.insert('好');
        assert_eq!(editor.text, "feat: café\n好");
    }

    #[test]
    fn ui_renders_small_and_normal_terminals_with_dialogs() {
        let repo = Repo {
            root: PathBuf::from("/nonexistent"),
        };
        let mut app = App::new(repo);
        for (w, h) in [(40, 10), (64, 18), (120, 36)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            for modal in [
                None,
                Some(Modal::Help),
                Some(Modal::Commit),
                Some(Modal::Error("test".into())),
            ] {
                let expected = if w < 64 || h < 18 {
                    "Resize"
                } else {
                    match &modal {
                        None => "jammygit",
                        Some(Modal::Help) => "Keys & behavior",
                        Some(Modal::Commit) => "Commit staged changes",
                        _ => "Cannot continue",
                    }
                };
                app.modal = modal;
                terminal.draw(|f| app.draw(f)).unwrap();
                let content: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(content.contains(expected), "missing {expected} at {w}x{h}");
            }
        }
    }
}
