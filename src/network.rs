//! Git operations in a private terminal, keeping prompts and output inside the TUI.
use std::{
    io::{Read, Write},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::git::{Action, Plan, Repo, Result};

pub enum Event {
    Output(Vec<u8>),
    Finished(Result<()>),
    InputError(String),
}

pub struct Job {
    events: Receiver<Event>,
    input: SyncSender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    finished: bool,
}

impl Job {
    pub fn start(repo: &Repo, plan: &Plan, rows: u16, cols: u16) -> Result<Self> {
        if !matches!(plan.action, Action::Commit(_) | Action::Push | Action::Pull) {
            return Err("Only commit, push, and pull run in the embedded Git terminal.".into());
        }
        repo.validate(plan)?;
        let pair = native_pty_system()
            .openpty(size(rows, cols))
            .map_err(|e| e.to_string())?;
        let mut command = CommandBuilder::new("git");
        command.cwd(&repo.root);
        command.args(["--no-pager", "--literal-pathspecs", "-c", "color.ui=false"]);
        command.args(&plan.args);
        command.env("TERM", "xterm-256color");
        #[cfg(unix)]
        if let Some(tty) = pair.master.tty_name() {
            // A shell's inherited GPG_TTY points at the outer terminal.
            command.env("GPG_TTY", tty);
        }
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        let killer = child.clone_killer();
        drop(pair.slave);
        // Bounded output plus bounded terminal scrollback keeps large pushes lightweight.
        let (sender, events) = mpsc::sync_channel(32);
        let (input, input_receiver) = mpsc::sync_channel::<Vec<u8>>(256);
        let input_events = sender.clone();
        thread::spawn(move || {
            while let Ok(bytes) = input_receiver.recv() {
                if let Err(error) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                    let _ = input_events.send(Event::InputError(format!(
                        "Cannot send input to Git: {error}"
                    )));
                    break;
                }
            }
        });
        let title = plan.title.clone();
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            let read_error = loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break None,
                    Ok(count) => {
                        if sender
                            .send(Event::Output(buffer[..count].to_vec()))
                            .is_err()
                        {
                            let _ = child.kill();
                            let _ = child.wait();
                            return;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    // Unix PTYs can report EIO when the slave closes, instead of EOF.
                    Err(error) if error.raw_os_error() == Some(5) => break None,
                    Err(error) => break Some(error.to_string()),
                }
            };
            if read_error.is_some() {
                let _ = child.kill();
            }
            let result = child
                .wait()
                .map_err(|e| format!("Cannot wait for Git: {e}"))
                .and_then(|status| {
                    if let Some(error) = read_error {
                        return Err(format!("Cannot read Git output: {error}"));
                    }
                    if status.success() {
                        Ok(())
                    } else {
                        Err(format!("{title} failed ({status}). Review the Git output."))
                    }
                });
            let _ = sender.send(Event::Finished(result));
        });
        Ok(Self {
            events,
            input,
            master: pair.master,
            killer,
            finished: false,
        })
    }

    pub fn poll(&mut self) -> Option<Event> {
        match self.events.try_recv() {
            Ok(event) => {
                if matches!(event, Event::Finished(_)) {
                    self.finished = true;
                }
                Some(event)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) if !self.finished => {
                self.finished = true;
                Some(Event::Finished(Err(
                    "The Git worker stopped unexpectedly.".into()
                )))
            }
            Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.input
            .try_send(bytes.to_vec())
            .map_err(|error| format!("Cannot queue input to Git: {error}"))
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master
            .resize(size(rows, cols))
            .map_err(|e| format!("Cannot resize Git terminal: {e}"))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.killer.kill();
        }
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}
