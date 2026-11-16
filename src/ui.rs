use crate::{App, Modal, nonzero};
use jammygit::git::{Entry, safe_text};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
    },
};

const BLUE: Color = Color::Rgb(35, 78, 145);
const ACCENT: Color = Color::Rgb(141, 182, 239);
const BG: Color = Color::Rgb(15, 20, 29);
const SURFACE: Color = Color::Rgb(21, 29, 42);
const SELECTED: Color = Color::Rgb(26, 51, 85);
const LINE: Color = Color::Rgb(45, 57, 76);
const TEXT: Color = Color::Rgb(232, 237, 245);
const DIM: Color = Color::Rgb(146, 160, 182);
const GREEN: Color = Color::Rgb(159, 204, 172);
const RED: Color = Color::Rgb(239, 159, 163);

impl App {
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Block::new().style(Style::default().fg(TEXT).bg(BG)), area);
        if area.width < 64 || area.height < 18 {
            frame.render_widget(
                Paragraph::new("jammygit\n\nResize to at least 64 × 18.\nq or Ctrl+C exits.")
                    .style(Style::default().fg(ACCENT)),
                area,
            );
            return;
        }
        let editing = matches!(self.modal, Some(Modal::Commit));
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(if editing { 7 } else { 3 }),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);
        self.draw_header(frame, rows[0]);
        let elapsed = self.drawer_changed.elapsed().as_secs_f32() / 0.12;
        let eased = 1.0 - (1.0 - elapsed.min(1.0)).powi(3);
        let target = if self.drawer_open { 1.0 } else { 0.0 };
        self.drawer_progress = self.drawer_from + (target - self.drawer_from) * eased;
        let drawer_target = if area.width < 95 {
            area.width
        } else {
            area.width * 3 / 5
        };
        let drawer_width = (f32::from(drawer_target) * self.drawer_progress).round() as u16;
        let lane = Rect::new(
            rows[1].x,
            rows[1].y,
            rows[1].width.saturating_sub(drawer_width),
            rows[1].height,
        );
        if lane.width >= 20 {
            self.draw_conversation(frame, centered(lane, 88));
        }
        if drawer_width > 1 {
            self.draw_diff(
                frame,
                Rect::new(
                    rows[1].right() - drawer_width,
                    rows[1].y,
                    drawer_width,
                    rows[1].height,
                ),
            );
        }
        self.draw_composer(frame, centered(rows[2], 88));
        frame.render_widget(
            Paragraph::new(safe_text(&self.notice))
                .style(Style::default().fg(if self.failed { RED } else { DIM }))
                .wrap(Wrap { trim: false }),
            centered(rows[3], 104),
        );
        let keys = if self.network.is_some() {
            "i input   Esc leave input   Ctrl+C interrupt Git   J/K scroll"
        } else if editing {
            "Enter commit   Esc cancel   Ctrl+J newline   Ctrl+U clear"
        } else if area.width < 90 {
            "↑↓ select  Enter diff  Space stage  c commit  P push  p pull"
        } else {
            "↑↓ select   Enter diff   Space stage   c commit   P push   p pull   ? help"
        };
        frame.render_widget(
            Paragraph::new(keys)
                .centered()
                .style(Style::default().fg(ACCENT)),
            rows[4],
        );
        self.draw_modal(frame);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let name = self
            .repo
            .root
            .file_name()
            .map(|s| safe_text(&s.to_string_lossy()))
            .unwrap_or_else(|| "repository".into());
        let branch = self
            .state
            .as_ref()
            .and_then(|s| s.branch.as_deref())
            .unwrap_or(if self.state.is_some() {
                "detached HEAD"
            } else {
                "loading…"
            });
        let mut line = vec![
            Span::styled(
                " jammygit ",
                Style::default().fg(Color::White).bg(BLUE).bold(),
            ),
            Span::styled(format!("  {name}"), Style::default().fg(TEXT).bold()),
            Span::styled(
                format!("  /  {}", safe_text(branch)),
                Style::default().fg(DIM),
            ),
        ];
        if let Some((ahead, behind)) = self.state.as_ref().and_then(|s| s.counts) {
            line.push(Span::styled(
                format!("  ↑{ahead} ↓{behind}"),
                Style::default().fg(ACCENT),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(line)).block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(LINE)),
            ),
            area,
        );
    }

    fn draw_conversation(&mut self, frame: &mut Frame, area: Rect) {
        let count = self.state.as_ref().map_or(0, |s| s.entries.len());
        let desired = (count as u16).saturating_mul(2).saturating_add(2).max(8);
        let (files, activity) = if self.show_output {
            let parts = Layout::vertical([
                Constraint::Length(desired.min(8).min(area.height / 3)),
                Constraint::Min(3),
            ])
            .split(area);
            (parts[0], Some(parts[1]))
        } else {
            let height = desired.min(area.height);
            let offset = area.height.saturating_sub(height) / 2;
            (Rect::new(area.x, area.y + offset, area.width, height), None)
        };
        self.draw_files(frame, files);
        if let Some(activity) = activity {
            self.draw_activity(frame, activity);
        }
    }

    fn draw_files(&mut self, frame: &mut Frame, area: Rect) {
        let entries = self.state.as_ref().map(|s| &s.entries);
        let count = entries.map_or(0, Vec::len);
        let staged = entries.map_or(0, |e| e.iter().filter(|e| e.staged()).count());
        let title = format!(" {count} changed files · {staged} staged ");
        if area.height < 5 {
            frame.render_widget(Paragraph::new(title).style(Style::default().fg(DIM)), area);
            return;
        }
        let block = card(&title);
        if count == 0 {
            frame.render_widget(
                Paragraph::new(if self.state.is_some() {
                    "\n  Your working tree is clean.\n  P pushes this branch. p pulls updates."
                } else if self.refresh_pending {
                    "\n  Reading repository…"
                } else {
                    "\n  Cannot read repository. Press r to retry."
                })
                .style(Style::default().fg(DIM))
                .block(block),
                area,
            );
            return;
        }
        let visible = usize::from(block.inner(area).height.div_ceil(2)).max(1);
        let selected = self.selection.selected().unwrap_or(0);
        let offset = self.selection.offset().min(selected);
        let offset = if selected >= offset + visible {
            selected + 1 - visible
        } else {
            offset
        };
        *self.selection.offset_mut() = offset;
        let mut local_selection =
            ListState::default().with_selected(Some(selected.saturating_sub(offset)));
        let items: Vec<ListItem> = entries
            .into_iter()
            .flatten()
            .skip(offset)
            .take(visible)
            .map(|entry| {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            if entry.staged() { " ●  " } else { " ○  " },
                            Style::default().fg(if entry.conflict() { RED } else { ACCENT }),
                        ),
                        Span::styled(entry.label(), Style::default().fg(TEXT)),
                    ]),
                    Line::from(Span::styled(
                        format!("    {}", file_state(entry)),
                        Style::default().fg(DIM),
                    )),
                ])
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_style(Style::default().bg(SELECTED).add_modifier(Modifier::BOLD))
                .highlight_symbol("▎"),
            area,
            &mut local_selection,
        );
    }

    fn draw_activity(&mut self, frame: &mut Frame, area: Rect) {
        let label = if self.network.is_some() {
            if self.network_input {
                "input · Esc to leave"
            } else {
                "running · i for input"
            }
        } else {
            "finished · o to hide"
        };
        let title = format!(" {} · {label} ", self.network_title);
        let block = card(&title);
        let body = block.inner(area);
        frame.render_widget(block, area);
        let size = (body.height.max(1), body.width.max(1));
        if size != self.output_size {
            self.output_size = size;
            if let Some(parser) = &mut self.network_output {
                parser.set_size(nonzero(size.0), nonzero(size.1));
            }
            if let Some(job) = &self.network
                && let Err(error) = job.resize(size.0, size.1)
            {
                self.notice = error;
                self.failed = true;
            }
        }
        if let Some(parser) = &self.network_output {
            frame.render_widget(
                Paragraph::new(parser.screen().contents()).style(Style::default().fg(TEXT)),
                body,
            );
            if self.network_input
                && !parser.screen().hide_cursor()
                && body.width > 0
                && body.height > 0
            {
                let (row, col) = parser.screen().cursor_position();
                frame.set_cursor_position((
                    body.x + col.min(body.width - 1),
                    body.y + row.min(body.height - 1),
                ));
            }
        }
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let block = Block::new()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(BLUE))
            .style(Style::default().fg(TEXT).bg(SURFACE))
            .padding(Padding::horizontal(1));
        let body = block.inner(area);
        frame.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(body);
        let name = if self.all_diff {
            "All staged files".into()
        } else {
            self.state
                .as_ref()
                .and_then(|s| self.selection.selected().and_then(|i| s.entries.get(i)))
                .map(|e| e.label())
                .unwrap_or_else(|| "Diff".into())
        };
        frame.render_widget(
            Paragraph::new(name).style(Style::default().fg(TEXT).bold()),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{} · Tab switch · Esc close",
                if self.staged {
                    "Staged"
                } else {
                    "Working changes"
                }
            ))
            .style(Style::default().fg(ACCENT)),
            rows[1],
        );
        let patch: Vec<Line> = self
            .diff
            .lines()
            .skip(usize::from(self.scroll))
            .take(usize::from(rows[2].height))
            .map(|line| {
                let color = if line.starts_with("+++") || line.starts_with("---") {
                    DIM
                } else if line.starts_with('+') {
                    GREEN
                } else if line.starts_with('-') {
                    RED
                } else if line.starts_with("@@") || line.starts_with("diff --git") {
                    ACCENT
                } else {
                    TEXT
                };
                Line::styled(safe_text(line), Style::default().fg(color))
            })
            .collect();
        frame.render_widget(Paragraph::new(patch).scroll((0, self.scroll_x)), rows[2]);
    }

    fn draw_composer(&self, frame: &mut Frame, area: Rect) {
        let editing = matches!(self.modal, Some(Modal::Commit));
        let block = card(if editing {
            " Commit staged changes "
        } else {
            " Commit "
        })
        .border_style(Style::default().fg(if editing { ACCENT } else { LINE }));
        let input = block.inner(area);
        frame.render_widget(block, area);
        if !editing {
            frame.render_widget(
                Paragraph::new(if self.network.is_some() {
                    "Git is running. Its output and prompts appear above."
                } else {
                    "Press c to write a commit message for your staged changes."
                })
                .style(Style::default().fg(DIM)),
                input,
            );
            return;
        }
        let before = &self.editor.text[..self.editor.cursor];
        let row = before.matches('\n').count() as u16;
        let col = Line::from(before.rsplit('\n').next().unwrap_or("")).width() as u16;
        let vscroll = row.saturating_sub(input.height.saturating_sub(1));
        let hscroll = col.saturating_sub(input.width.saturating_sub(1));
        frame.render_widget(
            Paragraph::new(self.editor.text.as_str())
                .style(Style::default().fg(TEXT))
                .scroll((vscroll, hscroll)),
            input,
        );
        if input.width > 0 && input.height > 0 {
            frame.set_cursor_position((input.x + col - hscroll, input.y + row - vscroll));
        }
    }

    fn draw_modal(&self, frame: &mut Frame) {
        let Some(modal) = &self.modal else {
            return;
        };
        if matches!(modal, Modal::Commit) {
            return;
        }
        let height = if matches!(modal, Modal::Help) { 25 } else { 14 };
        let width = frame.area().width.saturating_sub(4).min(84);
        let height = height.min(frame.area().height.saturating_sub(2));
        let area = Rect::new(
            (frame.area().width - width) / 2,
            (frame.area().height - height) / 2,
            width,
            height,
        );
        let (title, text) = match modal {
            Modal::Help => (" Keys & behavior ", "↑ / ↓ or j / k   Select a file\nEnter / →        Slide in the selected diff\nEsc / ←          Close the diff drawer\nSpace / u        Stage / unstage the selected file\na                Stage all (with confirmation)\nTab              Switch working / staged diff\nv                Switch selected file / all files\nJ / K            Scroll diff or Git output\nh / l            Pan the diff horizontally\nc                Write a commit in the composer\nP / p            Push / pull immediately\no                Show / hide Git activity\ng                Recent commits\nr                Refresh\nq / Ctrl+C       Quit when Git is idle\n\nComposer: Enter commits; Ctrl+J adds a newline; Esc cancels.\nCommits, pushes, and pulls run inside the app.\nWhile Git runs: i types into its terminal; Esc leaves input.\nCtrl+C interrupts Git without leaving the app.\n\nPress any key to close.".into()),
            Modal::History => (" Recent commits ", format!("{}\n\nPress any key to close.", self.log.lines().map(safe_text).collect::<Vec<_>>().join("\n"))),
            Modal::Error(text) => (" Cannot continue ", format!("{}\n\nPress any key to close.", safe_text(text))),
            Modal::Confirm(plan) => (&*plan.title, format!("{}\n\nEnter / y to continue · Esc / n to cancel", plan.detail.lines().map(safe_text).collect::<Vec<_>>().join("\n"))),
            Modal::Commit => unreachable!(),
        };
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(TEXT).bg(SURFACE))
                .wrap(Wrap { trim: false })
                .block(card(title)),
            area,
        );
    }
}

fn centered(area: Rect, max_width: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(max_width);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y,
        width,
        area.height,
    )
}
fn card(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(title, Style::default().fg(ACCENT)))
        .border_style(Style::default().fg(LINE))
        .style(Style::default().bg(SURFACE))
        .padding(Padding::horizontal(1))
}
fn file_state(entry: &Entry) -> String {
    if entry.conflict() {
        return "Conflict · resolve before committing".into();
    }
    if entry.index == b'?' {
        return "New file · untracked".into();
    }
    let action = if entry.index == b'D' || entry.worktree == b'D' {
        "Deleted"
    } else if entry.original.is_some() {
        "Renamed"
    } else if entry.index == b'A' {
        "Added"
    } else {
        "Modified"
    };
    let stage = if entry.staged() && entry.unstaged() {
        "partly staged"
    } else if entry.staged() {
        "staged"
    } else {
        "unstaged"
    };
    format!("{action} · {stage}")
}
#[cfg(test)]
mod preview {
    use super::*;
    use jammygit::git::{Entry, Repo, Snapshot};
    use ratatui::{Terminal, backend::TestBackend};
    use std::{fs, io::Write, path::PathBuf};

    #[test]
    #[ignore = "exports actual Ratatui cells for visual review"]
    fn export_preview() {
        let directory = PathBuf::from(
            std::env::var_os("JAMMYGIT_PREVIEW_DIR").expect("Set JAMMYGIT_PREVIEW_DIR"),
        );
        fs::create_dir_all(&directory).unwrap();
        let mut app = App::new(Repo {
            root: PathBuf::from("/workspace/jammygit"),
        });
        app.state = Some(Snapshot {
            branch: Some("codex/blue-workspace".into()),
            head: Some("1b2a8c4".into()),
            upstream: Some("origin/codex/blue-workspace".into()),
            counts: Some((2, 0)),
            operation: None,
            entries: vec![
                Entry {
                    index: b'M',
                    worktree: b' ',
                    path: "src/ui.rs".into(),
                    original: None,
                },
                Entry {
                    index: b' ',
                    worktree: b'M',
                    path: "README.md".into(),
                    original: None,
                },
                Entry {
                    index: b'?',
                    worktree: b'?',
                    path: "docs/preview.png".into(),
                    original: None,
                },
            ],
        });
        app.selection.select(Some(0));
        app.failed = false;
        app.staged = true;
        app.diff = "diff --git a/src/ui.rs b/src/ui.rs\nindex 3e8a5c1..7b3a9f2 100644\n--- a/src/ui.rs\n+++ b/src/ui.rs\n@@ -12,6 +12,10 @@ use ratatui::style::Color;\n \n-const ACCENT: Color = Color::Rgb(97, 213, 191);\n-const BG: Color = Color::Rgb(20, 25, 33);\n+// Initium blue, with a navy workspace and lighter labels.\n+const BLUE: Color = Color::Rgb(35, 78, 145);\n+const ACCENT: Color = Color::Rgb(145, 184, 240);\n+const BG: Color = Color::Rgb(14, 23, 38);\n+const SURFACE: Color = Color::Rgb(20, 33, 53);\n \n fn draw_workspace(frame: &mut Frame) {\n+    draw_review(frame);\n+    draw_file_dock(frame);\n }".into();
        app.log = "1b2a8c4  feat: keep push and pull inside the app\n7d4e2a1  fix: preserve draft after hook failure\n3c8f9b2  feat: stage and review changes".into();
        app.notice = "1 file staged. Review the patch, then press c to commit.".into();
        for (name, width, height) in [
            ("workspace", 120, 38),
            ("compact", 64, 18),
            ("diff", 120, 38),
            ("compact-diff", 64, 18),
            ("commit", 120, 38),
            ("git-output", 120, 38),
        ] {
            app.drawer_open = name == "diff" || name == "compact-diff";
            app.drawer_progress = if app.drawer_open { 1.0 } else { 0.0 };
            app.drawer_from = app.drawer_progress;
            app.drawer_changed = std::time::Instant::now();
            app.modal = if name == "commit" {
                Some(Modal::Commit)
            } else {
                None
            };
            if name == "commit" {
                app.editor.text = "feat: add Initium blue workspace".into();
                app.editor.cursor = app.editor.text.len();
            }
            if name == "git-output" {
                let mut parser = vt100::Parser::new(nonzero(24), nonzero(118), 1000);
                parser.process(b"codex/blue-workspace -> origin/refs/heads/codex/blue-workspace\r\nDestination: git@example.com:initium/jammygit.git\r\n\r\nEnumerating objects: 14, done.\r\nCounting objects: 100% (14/14), done.\r\nWriting objects: 100% (8/8), 2.41 KiB, done.\r\n\r\nPush branch succeeded.\r\n");
                app.network_output = Some(parser);
                app.show_output = true;
                app.network_title = "Push branch".into();
                app.notice =
                    "Push branch succeeded. Tab returns to diff · o shows Git output".into();
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let mut output = fs::File::create(directory.join(format!("{name}.cells"))).unwrap();
            writeln!(output, "{width}\t{height}").unwrap();
            for y in 0..height {
                for x in 0..width {
                    let cell = &terminal.backend().buffer()[(x, y)];
                    let rgb = |color, fallback| {
                        let Color::Rgb(r, g, b) = color else {
                            return fallback;
                        };
                        format!("{r},{g},{b}")
                    };
                    writeln!(
                        output,
                        "{x}\t{y}\t{}\t{}\t{}\t{}",
                        rgb(cell.fg, "232,237,245".into()),
                        rgb(cell.bg, "14,23,38".into()),
                        u8::from(cell.modifier.contains(Modifier::BOLD)),
                        cell.symbol()
                    )
                    .unwrap();
                }
            }
        }
    }
}
