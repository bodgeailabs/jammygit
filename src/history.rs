//! Bounded, in-memory conversation history. No terminal output is written to disk.
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

const MAX_TURNS: usize = 100;
const MAX_OUTPUT_LINES: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Preparing,
    Running,
    Succeeded,
    Failed,
}
impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing…",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Copy)]
pub enum Kind {
    RequestLabel,
    Request,
    Response(Status),
    Output,
    Space,
}
pub struct Row {
    pub text: String,
    pub kind: Kind,
}
pub struct Turn {
    pub id: u64,
    pub title: String,
    pub request: String,
    pub status: Status,
    pub output: Vec<String>,
    pub rows: Vec<Row>,
    width: u16,
    dirty: bool,
}
impl Turn {
    pub fn prepare(&mut self, width: u16) {
        if self.width == width && !self.dirty {
            return;
        }
        self.width = width;
        self.dirty = false;
        self.rows.clear();
        self.rows.push(Row {
            text: format!("You · {}", self.title),
            kind: Kind::RequestLabel,
        });
        for text in wrap(&self.request, width.saturating_sub(4).max(1)) {
            self.rows.push(Row {
                text,
                kind: Kind::Request,
            });
        }
        self.rows.push(Row {
            text: String::new(),
            kind: Kind::Space,
        });
        self.rows.push(Row {
            text: format!("Git · {}", self.status.label()),
            kind: Kind::Response(self.status),
        });
        for line in &self.output {
            for text in wrap(line, width.max(1)) {
                self.rows.push(Row {
                    text,
                    kind: Kind::Output,
                });
            }
        }
        if matches!(self.status, Status::Preparing) {
            self.rows.push(Row {
                text: "Checking repository state…".into(),
                kind: Kind::Output,
            });
        }
    }
}

#[derive(Default)]
pub struct Transcript {
    pub turns: VecDeque<Turn>,
    pub trimmed: bool,
    next_id: u64,
    top: Option<usize>,
    total: usize,
    viewport: usize,
}
impl Transcript {
    pub fn begin(&mut self, title: String, request: String) -> u64 {
        self.next_id += 1;
        self.turns.push_back(Turn {
            id: self.next_id,
            title,
            request,
            status: Status::Preparing,
            output: Vec::new(),
            rows: Vec::new(),
            width: 0,
            dirty: true,
        });
        self.follow_latest();
        self.trim();
        self.next_id
    }
    pub fn running(&mut self, id: u64, title: String) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == id) {
            turn.title = title;
            turn.status = Status::Running;
            turn.dirty = true;
        }
    }
    pub fn finish(&mut self, id: u64, success: bool, output: Vec<String>) {
        if let Some(turn) = self.turns.iter_mut().find(|t| t.id == id) {
            turn.output = output;
            turn.status = if success {
                Status::Succeeded
            } else {
                Status::Failed
            };
            turn.dirty = true;
        }
        self.trim();
    }
    fn trim(&mut self) {
        let mut lines: usize = self.turns.iter().map(|turn| turn.output.len()).sum();
        while self.turns.len() > 1 && (self.turns.len() > MAX_TURNS || lines > MAX_OUTPUT_LINES) {
            let turn = self.turns.pop_front().unwrap();
            lines -= turn.output.len();
            if let Some(top) = &mut self.top {
                *top = top.saturating_sub(turn.rows.len() + 2);
            }
            self.trimmed = true;
        }
    }
    pub fn viewport(&mut self, total: usize, height: u16) -> usize {
        self.total = total;
        self.viewport = usize::from(height);
        let bottom = total.saturating_sub(self.viewport);
        if let Some(top) = &mut self.top {
            *top = (*top).min(bottom);
        }
        self.top.unwrap_or(bottom)
    }
    pub fn scroll(&mut self, up: bool, lines: usize) {
        let bottom = self.total.saturating_sub(self.viewport);
        let current = self.top.unwrap_or(bottom);
        let next = if up {
            current.saturating_sub(lines)
        } else {
            current.saturating_add(lines).min(bottom)
        };
        self.top = if next >= bottom { None } else { Some(next) };
    }
    pub fn oldest(&mut self) {
        self.top = Some(0);
    }
    pub fn follow_latest(&mut self) {
        self.top = None;
    }
    pub fn following(&self) -> bool {
        self.top.is_none()
    }
}

fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut current = String::new();
        let mut used = 0;
        for ch in line.chars() {
            let size = ch.width().unwrap_or(0);
            if used + size > width && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(ch);
            used += size;
        }
        rows.push(current);
    }
    rows
}

/// Include scrollback, while touching only the requested terminal rows.
pub fn terminal_rows(parser: &mut vt100::Parser, start: usize, count: usize) -> Vec<String> {
    let screen = parser.screen_mut();
    let saved = screen.scrollback();
    let history = screen.scrollback_len();
    let (rows, cols) = screen.size();
    let mut position = start;
    let mut output = Vec::new();
    while output.len() < count {
        let (offset, skip) = if position < history {
            (history - position, 0)
        } else {
            (0, position - history)
        };
        if skip >= usize::from(rows.get()) {
            break;
        }
        screen.set_scrollback(offset);
        let page: Vec<String> = screen
            .rows(0, cols.get())
            .skip(skip)
            .take(count - output.len())
            .collect();
        if page.is_empty() {
            break;
        }
        position += page.len();
        output.extend(page);
    }
    screen.set_scrollback(saved);
    output
}
pub fn terminal_height(parser: &mut vt100::Parser) -> usize {
    let screen = parser.screen_mut();
    let saved = screen.scrollback();
    screen.set_scrollback(0);
    let (_, cols) = screen.size();
    let visible = screen
        .rows(0, cols.get())
        .enumerate()
        .filter(|(_, row)| !row.is_empty())
        .map(|(i, _)| i + 1)
        .last()
        .unwrap_or(0)
        .max(usize::from(screen.cursor_position().0) + 1);
    let height = screen.scrollback_len() + visible;
    screen.set_scrollback(saved);
    height
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scrolling_back_stays_anchored_when_more_output_arrives() {
        let mut history = Transcript::default();
        assert_eq!(history.viewport(100, 20), 80);
        history.scroll(true, 10);
        assert_eq!(history.viewport(150, 20), 70);
        history.scroll(false, 80);
        assert!(history.following());
        assert_eq!(history.viewport(160, 20), 140);
    }
    #[test]
    fn terminal_history_captures_earlier_pages_in_order() {
        let mut parser = vt100::Parser::new(crate::nonzero(3), crate::nonzero(20), 1000);
        for n in 0..12 {
            parser.process(format!("line {n}\r\n").as_bytes());
        }
        let height = terminal_height(&mut parser);
        let lines = terminal_rows(&mut parser, 0, height);
        assert!(lines.len() >= 12);
        for (n, line) in lines.iter().take(12).enumerate() {
            assert_eq!(line, &format!("line {n}"));
        }
        assert_eq!(parser.screen().scrollback(), 0);
    }
    #[test]
    fn bounded_history_keeps_the_latest_actions_and_unicode_wraps() {
        let mut history = Transcript::default();
        for _ in 0..120 {
            let id = history.begin("Commit".into(), "你好 café".into());
            history.finish(id, true, vec!["done".into()]);
        }
        assert_eq!(history.turns.len(), MAX_TURNS);
        assert!(history.trimmed);
        let turn = history.turns.back_mut().unwrap();
        turn.prepare(10);
        assert_eq!(turn.status, Status::Succeeded);
        assert!(turn.rows.iter().any(|row| row.text.contains("你好")));
    }
}
