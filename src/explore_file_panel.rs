use std::path::Path;

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Paragraph},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileAction {
    Stay,
    Pass,
    Close,
    Activate(usize),
    Toggle(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileLoadState {
    Ready,
    Unopened,
    Loading,
    Failed,
}

pub(crate) struct FileRow<'a> {
    pub label: &'a Path,
    pub enabled: bool,
    pub state: FileLoadState,
    pub has_audit: bool,
}

pub(crate) struct FilePanel {
    pub visible: bool,
    cursor: usize,
    first: usize,
    page_rows: usize,
    list_rows: Rect,
    panel_rect: Rect,
}

impl FilePanel {
    pub(crate) fn new() -> Self {
        Self {
            visible: false,
            cursor: 0,
            first: 0,
            page_rows: 1,
            list_rows: Rect::default(),
            panel_rect: Rect::default(),
        }
    }

    pub(crate) fn set_cursor(&mut self, index: usize, count: usize) {
        self.cursor = index.min(count.saturating_sub(1));
        if count == 0 {
            self.first = 0;
        }
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        if self.cursor < self.first {
            self.first = self.cursor;
        } else if self.cursor >= self.first.saturating_add(self.page_rows) {
            self.first = self.cursor + 1 - self.page_rows;
        }
    }

    fn move_cursor(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.set_cursor(0, 0);
            return;
        }
        let next = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as usize)
        };
        self.set_cursor(next, count);
    }

    pub(crate) fn key(&mut self, key: KeyCode, count: usize) -> FileAction {
        self.set_cursor(self.cursor, count);
        match key {
            KeyCode::Char('g' | 'q') | KeyCode::Esc => FileAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1, count);
                FileAction::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1, count);
                FileAction::Stay
            }
            KeyCode::PageUp => {
                self.move_cursor(-(self.page_rows as isize), count);
                FileAction::Stay
            }
            KeyCode::PageDown => {
                self.move_cursor(self.page_rows as isize, count);
                FileAction::Stay
            }
            KeyCode::Home => {
                self.set_cursor(0, count);
                FileAction::Stay
            }
            KeyCode::End => {
                self.set_cursor(count.saturating_sub(1), count);
                FileAction::Stay
            }
            KeyCode::Enter if count > 0 => FileAction::Activate(self.cursor),
            KeyCode::Char(' ') if count > 0 => FileAction::Toggle(self.cursor),
            KeyCode::Enter | KeyCode::Char(' ') => FileAction::Stay,
            _ => FileAction::Pass,
        }
    }

    pub(crate) fn click(&mut self, x: u16, y: u16, count: usize) -> FileAction {
        if !contains(self.panel_rect, x, y) {
            return FileAction::Pass;
        }
        if !contains(self.list_rows, x, y) {
            return FileAction::Stay;
        }
        let index = self.first + usize::from(y - self.list_rows.y);
        if index >= count {
            return FileAction::Stay;
        }
        self.set_cursor(index, count);
        if x >= self.list_rows.x.saturating_add(2) && x < self.list_rows.x.saturating_add(5) {
            FileAction::Toggle(index)
        } else {
            FileAction::Activate(index)
        }
    }

    pub(crate) fn render<'a>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        count: usize,
        active: usize,
        mut rows: impl FnMut(usize) -> FileRow<'a>,
    ) {
        self.panel_rect = area;
        self.list_rows = Rect::default();
        if area.width == 0 || area.height == 0 {
            return;
        }
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Raw files"),
            area,
        );
        let inside = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if inside.width == 0 || inside.height < 4 {
            return;
        }
        frame.render_widget(
            Paragraph::new("A audit  · new  … load  ! error")
                .style(Style::default().fg(Color::Cyan)),
            Rect::new(inside.x, inside.y, inside.width, 1),
        );
        frame.render_widget(
            Paragraph::new("Enter open  Space toggle"),
            Rect::new(inside.x, inside.bottom() - 2, inside.width, 1),
        );
        frame.render_widget(
            Paragraph::new("↑↓ select  g/Esc close"),
            Rect::new(inside.x, inside.bottom() - 1, inside.width, 1),
        );
        self.list_rows = Rect::new(inside.x, inside.y + 1, inside.width, inside.height - 3);
        self.page_rows = usize::from(self.list_rows.height);
        self.set_cursor(self.cursor, count);
        if self.list_rows.width < 10 {
            return;
        }
        if count == 0 {
            frame.render_widget(
                Paragraph::new("No raw MS files"),
                Rect::new(self.list_rows.x, self.list_rows.y, self.list_rows.width, 1),
            );
            return;
        }
        for offset in 0..self.page_rows.min(count - self.first) {
            let index = self.first + offset;
            let row = rows(index);
            let y = self.list_rows.y + offset as u16;
            let base = Style::default().fg(Color::White);
            let base = if index == self.cursor {
                base.bg(Color::DarkGray)
            } else {
                base
            };
            let line = Rect::new(self.list_rows.x, y, self.list_rows.width, 1);
            frame.buffer_mut().set_style(line, base);
            let buf = frame.buffer_mut();
            let x = line.x;
            buf.set_string(
                x,
                y,
                if index == active { "▶" } else { " " },
                base.fg(Color::Green),
            );
            buf.set_string(x + 2, y, if row.enabled { "[x]" } else { "[ ]" }, base);
            buf.set_string(x + 6, y, "■", base.fg(file_color(index)));
            let label = row.label.to_string_lossy();
            let label_width = line.width.saturating_sub(11);
            let label_style = if row.state != FileLoadState::Ready {
                base.fg(Color::Gray)
            } else if index == active {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            };
            frame.render_widget(
                Paragraph::new(label.as_ref()).style(label_style),
                Rect::new(x + 8, y, label_width, 1),
            );
            let audit_x = line.right() - 3;
            frame.buffer_mut().set_string(
                audit_x,
                y,
                if row.has_audit { "A" } else { "-" },
                base.fg(if row.has_audit {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            );
            let (symbol, color) = match row.state {
                FileLoadState::Ready => (" ", Color::White),
                FileLoadState::Unopened => ("·", Color::Gray),
                FileLoadState::Loading => ("…", Color::Cyan),
                FileLoadState::Failed => ("!", Color::Red),
            };
            frame
                .buffer_mut()
                .set_string(line.right() - 1, y, symbol, base.fg(color));
        }
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

/// A fixed, allocation-free color wheel shared by the graph and file swatches.
/// The first 32 indices have distinct, bright RGB values on dark terminals.
pub(crate) fn file_color(index: usize) -> Color {
    let hue = (index.wrapping_mul(137) % 360) as u16;
    let sector = hue / 60;
    let rise = (192 * (hue % 60) / 60) as u8 + 63;
    let fall = (192 * (60 - hue % 60) / 60) as u8 + 63;
    match sector {
        0 => Color::Rgb(255, rise, 63),
        1 => Color::Rgb(fall, 255, 63),
        2 => Color::Rgb(63, 255, rise),
        3 => Color::Rgb(63, fall, 255),
        4 => Color::Rgb(rise, 63, 255),
        _ => Color::Rgb(255, 63, fall),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::Path};

    use crossterm::event::KeyCode;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};

    use super::{FileAction, FileLoadState, FilePanel, FileRow, file_color};

    fn render(panel: &mut FilePanel, count: usize, active: usize) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
        terminal
            .draw(|frame| {
                panel.render(frame, Rect::new(0, 0, 30, 12), count, active, |_| FileRow {
                    label: Path::new("sample/very-long-relative-raw-file-name.mzML"),
                    enabled: true,
                    state: FileLoadState::Ready,
                    has_audit: false,
                });
            })
            .unwrap();
        terminal
    }

    #[test]
    fn navigation_selects_and_activates_without_stealing_global_keys() {
        let mut panel = FilePanel::new();
        assert_eq!(panel.key(KeyCode::Enter, 0), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Char(' '), 0), FileAction::Stay);
        panel.set_cursor(2, 4);
        assert_eq!(panel.key(KeyCode::Char(' '), 4), FileAction::Toggle(2));
        assert_eq!(panel.key(KeyCode::Down, 4), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Enter, 4), FileAction::Activate(3));
        assert_eq!(panel.key(KeyCode::Char('j'), 4), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Enter, 4), FileAction::Activate(3));
        assert_eq!(panel.key(KeyCode::Home, 4), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Char('k'), 4), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Enter, 4), FileAction::Activate(0));
        assert_eq!(panel.key(KeyCode::End, 4), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Char(' '), 4), FileAction::Toggle(3));
        for key in ['f', 'n', 'b', 'o', 'v', 'i', 'p', '/'] {
            assert_eq!(panel.key(KeyCode::Char(key), 4), FileAction::Pass);
        }
        for key in [KeyCode::Char('g'), KeyCode::Char('q'), KeyCode::Esc] {
            assert_eq!(panel.key(key, 4), FileAction::Close);
        }
    }

    #[test]
    fn page_navigation_scrolls_and_mouse_targets_visible_rows() {
        let mut panel = FilePanel::new();
        render(&mut panel, 30, 0);
        assert_eq!(panel.key(KeyCode::PageDown, 30), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Enter, 30), FileAction::Activate(7));
        render(&mut panel, 30, 0);
        assert_eq!(panel.click(3, 2, 30), FileAction::Toggle(1));
        assert_eq!(panel.click(8, 8, 30), FileAction::Activate(7));
        assert_eq!(panel.key(KeyCode::PageDown, 30), FileAction::Stay);
        assert_eq!(panel.key(KeyCode::Enter, 30), FileAction::Activate(14));
        render(&mut panel, 30, 0);
        assert_eq!(panel.click(8, 2, 30), FileAction::Activate(8));
        assert_eq!(panel.click(8, 10, 30), FileAction::Stay);
        assert_eq!(panel.click(31, 2, 30), FileAction::Pass);
        panel.set_cursor(29, 30);
        render(&mut panel, 30, 0);
        assert_eq!(panel.click(8, 2, 30), FileAction::Activate(23));
    }

    #[test]
    fn row_distinguishes_unopened_loading_and_failed_files() {
        let mut panel = FilePanel::new();
        let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
        terminal
            .draw(|frame| {
                panel.render(frame, Rect::new(0, 0, 30, 12), 4, 0, |index| FileRow {
                    label: Path::new("samples/long-raw-file-name.mzXML"),
                    enabled: index == 0,
                    state: match index {
                        0 => FileLoadState::Ready,
                        1 => FileLoadState::Unopened,
                        2 => FileLoadState::Loading,
                        _ => FileLoadState::Failed,
                    },
                    has_audit: index == 0,
                });
            })
            .unwrap();
        let cells = terminal.backend().buffer();
        assert_eq!(cells.cell((1, 2)).unwrap().symbol(), "▶");
        assert_eq!(cells.cell((3, 2)).unwrap().symbol(), "[");
        assert_eq!(cells.cell((4, 2)).unwrap().symbol(), "x");
        assert_eq!(cells.cell((26, 2)).unwrap().symbol(), "A");
        assert_eq!(cells.cell((4, 3)).unwrap().symbol(), " ");
        assert_eq!(cells.cell((26, 3)).unwrap().symbol(), "-");
        assert_eq!(cells.cell((27, 3)).unwrap().symbol(), " ");
        assert_eq!(cells.cell((28, 3)).unwrap().symbol(), "·");
        assert_eq!(cells.cell((28, 4)).unwrap().symbol(), "…");
        assert_eq!(cells.cell((28, 5)).unwrap().symbol(), "!");
        assert_eq!(cells.cell((7, 2)).unwrap().fg, file_color(0));
    }

    #[test]
    fn long_labels_fit_both_sidebar_sizes_and_scrolled_cursor_stays_visible() {
        for (width, height, side_width) in [(80, 24, 29), (120, 36, 39)] {
            let mut panel = FilePanel::new();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let side = Rect::new(width - side_width, 0, side_width, height);
            panel.set_cursor(99, 100);
            terminal
                .draw(|frame| {
                    panel.render(frame, side, 100, 99, |_| FileRow {
                        label: Path::new("experiment/replicate/very-long-file-name.mzML"),
                        enabled: false,
                        state: FileLoadState::Ready,
                        has_audit: true,
                    });
                })
                .unwrap();
            let cells = terminal.backend().buffer();
            let row_x = side.x + 1;
            assert_eq!(cells.cell((row_x, 2)).unwrap().symbol(), "▶");
            assert_eq!(cells.cell((row_x + 8, 2)).unwrap().symbol(), "e");
            assert_eq!(cells.cell((side.right() - 1, 2)).unwrap().symbol(), "│",);
            assert_eq!(panel.key(KeyCode::Enter, 100), FileAction::Activate(99));
            assert_eq!(panel.click(row_x + 8, 2, 100), FileAction::Activate(99));
        }
    }

    #[test]
    fn first_32_graph_colors_are_distinct_truecolor() {
        let mut colors = HashSet::new();
        for index in 0..32 {
            let color = file_color(index);
            assert!(matches!(color, Color::Rgb(_, _, _)));
            assert!(colors.insert(color), "duplicate color for file {index}");
        }
    }
}
