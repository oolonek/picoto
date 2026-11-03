use std::{cmp::Ordering, fmt::Write};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{explore_data::Dataset, feature_list_view::FeatureList};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FeatureColumn {
    Id,
    Mz,
    Rt,
    Height,
    Area,
    Isotopes,
    Score,
}

impl FeatureColumn {
    const ALL: [Self; 7] = [
        Self::Id,
        Self::Mz,
        Self::Rt,
        Self::Height,
        Self::Area,
        Self::Isotopes,
        Self::Score,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::Mz => "m/z",
            Self::Rt => "RT",
            Self::Height => "Height",
            Self::Area => "Area",
            Self::Isotopes => "Iso",
            Self::Score => "Score",
        }
    }

    fn width(self) -> u16 {
        match self {
            Self::Id => 4,
            Self::Mz => 9,
            Self::Rt | Self::Score => 5,
            Self::Height | Self::Area => 7,
            Self::Isotopes => 3,
        }
    }
}

#[derive(Clone, Copy)]
struct Measurements {
    height: Option<f64>,
    area: Option<f64>,
    isotopes: Option<u64>,
    score: Option<f64>,
}

fn compare_available<T: PartialOrd>(
    left: Option<T>,
    right: Option<T>,
    descending: bool,
) -> Ordering {
    match (left, right) {
        (Some(a), Some(b)) => {
            let order = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub enum PanelAction {
    Stay,
    Pass,
    Close,
    Xic { index: usize, mz: f64, rt: f64 },
    Ms2 { index: usize, rt: f64 },
}

pub struct FeaturePanel {
    list: FeatureList,
    measurements: Vec<Measurements>,
    sort_column: FeatureColumn,
    descending: bool,
    matches: Vec<usize>,
    cursor: usize,
    first: usize,
    page_rows: usize,
    list_rows: Rect,
    column_header: Rect,
    detail_rect: Rect,
    detail_scroll: u16,
    query: String,
    editing: bool,
    error: Option<String>,
    pub visible: bool,
}

impl FeaturePanel {
    pub fn new(list: FeatureList) -> Self {
        let matches = (0..list.rows.len()).collect();
        let column = |name: &str| list.headers.iter().position(|header| header == name);
        let height_col = column("peak_height");
        let area_col = column("peak_area");
        let isotopes_col = column("isotope_peaks");
        let score_col = column("quality_score");
        let measurements = list
            .rows
            .iter()
            .map(|row| {
                let number = |index: Option<usize>| {
                    index
                        .and_then(|index| row.values[index].parse::<f64>().ok())
                        .filter(|value| value.is_finite())
                };
                Measurements {
                    height: number(height_col),
                    area: number(area_col),
                    isotopes: isotopes_col.and_then(|index| row.values[index].parse::<u64>().ok()),
                    score: number(score_col),
                }
            })
            .collect();
        let mut panel = Self {
            list,
            measurements,
            sort_column: FeatureColumn::Id,
            descending: false,
            matches,
            cursor: 0,
            first: 0,
            page_rows: 1,
            list_rows: Rect::default(),
            column_header: Rect::default(),
            detail_rect: Rect::default(),
            detail_scroll: 0,
            query: String::new(),
            editing: false,
            error: None,
            visible: false,
        };
        panel.sort_matches();
        panel
    }

    fn sort_matches(&mut self) {
        let rows = &self.list.rows;
        let metrics = &self.measurements;
        let descending = self.descending;
        let column = self.sort_column;
        self.matches.sort_unstable_by(|&left, &right| {
            let a = &rows[left];
            let b = &rows[right];
            let am = metrics[left];
            let bm = metrics[right];
            let order = match column {
                FeatureColumn::Id => compare_available(Some(a.id), Some(b.id), descending),
                FeatureColumn::Mz => compare_available(Some(a.mz), Some(b.mz), descending),
                FeatureColumn::Rt => {
                    compare_available(Some(a.rt_seconds), Some(b.rt_seconds), descending)
                }
                FeatureColumn::Height => compare_available(am.height, bm.height, descending),
                FeatureColumn::Area => compare_available(am.area, bm.area, descending),
                FeatureColumn::Isotopes => compare_available(am.isotopes, bm.isotopes, descending),
                FeatureColumn::Score => compare_available(am.score, bm.score, descending),
            };
            order.then_with(|| a.id.cmp(&b.id))
        });
    }

    fn sort_by(&mut self, column: FeatureColumn) {
        if self.sort_column == column {
            self.descending = !self.descending;
        } else {
            self.sort_column = column;
            self.descending = false;
        }
        self.sort_matches();
        self.cursor = 0;
        self.first = 0;
        self.detail_scroll = 0;
    }

    fn refresh(&mut self) {
        match self.list.filter(&self.query) {
            Ok(matches) => {
                self.matches = matches;
                self.error = None;
            }
            Err(error) => {
                self.matches.clear();
                self.error = Some(error.to_string());
            }
        }
        self.sort_matches();
        self.cursor = 0;
        self.first = 0;
        self.detail_scroll = 0;
    }

    fn keep_visible(&mut self) {
        if self.cursor < self.first {
            self.first = self.cursor;
        } else if self.cursor >= self.first + self.page_rows {
            self.first = self.cursor + 1 - self.page_rows;
        }
    }

    fn move_row(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as usize).min(last)
        };
        self.detail_scroll = 0;
        self.keep_visible();
    }

    fn selected(&self) -> Option<&crate::feature_list_view::FeatureRow> {
        self.matches
            .get(self.cursor)
            .map(|&index| &self.list.rows[index])
    }

    pub fn click(&mut self, column: u16, row: u16) {
        if row == self.column_header.y
            && column >= self.column_header.x
            && column < self.column_header.right()
        {
            let mut x = self.column_header.x;
            for field in FeatureColumn::ALL {
                if column >= x && column < x + field.width() {
                    self.sort_by(field);
                    break;
                }
                x += field.width() + 1;
            }
            return;
        }
        if column >= self.list_rows.x
            && column < self.list_rows.right()
            && row >= self.list_rows.y
            && row < self.list_rows.bottom()
        {
            let index = self.first + usize::from(row - self.list_rows.y);
            if index < self.matches.len() {
                self.cursor = index;
                self.detail_scroll = 0;
            }
        }
    }

    pub fn scroll(&mut self, column: u16, row: u16, up: bool) {
        let detail = column >= self.detail_rect.x
            && column < self.detail_rect.right()
            && row >= self.detail_rect.y
            && row < self.detail_rect.bottom();
        if detail {
            self.detail_scroll = if up {
                self.detail_scroll.saturating_sub(3)
            } else {
                self.detail_scroll.saturating_add(3)
            };
        } else {
            self.move_row(if up { -3 } else { 3 });
        }
    }

    pub fn key(&mut self, key: KeyCode) -> PanelAction {
        if self.editing {
            match key {
                KeyCode::Enter | KeyCode::Esc => self.editing = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.refresh();
                }
                KeyCode::Char(c) if !c.is_control() && self.query.len() < 160 => {
                    self.query.push(c);
                    self.refresh();
                }
                _ => {}
            }
            return PanelAction::Stay;
        }
        match key {
            KeyCode::Char('f' | 'q') | KeyCode::Esc => PanelAction::Close,
            KeyCode::Char('/') => {
                self.editing = true;
                PanelAction::Stay
            }
            KeyCode::Char('c') => {
                self.query.clear();
                self.refresh();
                PanelAction::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_row(-1);
                PanelAction::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_row(1);
                PanelAction::Stay
            }
            KeyCode::PageUp => {
                self.move_row(-(self.page_rows as isize));
                PanelAction::Stay
            }
            KeyCode::PageDown => {
                self.move_row(self.page_rows as isize);
                PanelAction::Stay
            }
            KeyCode::Home => {
                self.move_row(-(self.matches.len() as isize));
                PanelAction::Stay
            }
            KeyCode::End => {
                self.move_row(self.matches.len() as isize);
                PanelAction::Stay
            }
            KeyCode::Left => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                PanelAction::Stay
            }
            KeyCode::Right => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
                PanelAction::Stay
            }
            KeyCode::Char('x') | KeyCode::Enter => {
                self.selected()
                    .map_or(PanelAction::Stay, |row| PanelAction::Xic {
                        index: row.feature_index,
                        mz: row.mz,
                        rt: row.rt_seconds,
                    })
            }
            KeyCode::Char('s' | '2') => {
                self.selected()
                    .map_or(PanelAction::Stay, |row| PanelAction::Ms2 {
                        index: row.feature_index,
                        rt: row.rt_seconds,
                    })
            }
            _ => PanelAction::Pass,
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, data: &Dataset, minutes: bool) {
        let header = Rect::new(area.x, area.y, area.width, 2);
        let footer = Rect::new(area.x, area.bottom() - 2, area.width, 2);
        let content_height = area.height - 4;
        let list_height = (content_height * 2 / 5).max(6);
        let list_rect = Rect::new(area.x, area.y + 2, area.width, list_height);
        let details = Rect::new(
            area.x,
            list_rect.bottom(),
            area.width,
            content_height - list_height,
        );
        let total = self.list.rows.len();
        let selected = if self.matches.is_empty() {
            0
        } else {
            self.cursor + 1
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Features {selected}/{} of {total}\n{}",
                self.matches.len(),
                self.error
                    .as_deref()
                    .unwrap_or_else(|| if self.query.is_empty() {
                        "/ filter  column:value"
                    } else {
                        self.query.as_str()
                    }),
            )),
            header,
        );

        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(format!(
                    "Accepted features | {} {}",
                    self.sort_column.label(),
                    if self.descending { "↓" } else { "↑" }
                )),
            list_rect,
        );
        let inside = Rect::new(
            list_rect.x + 1,
            list_rect.y + 1,
            list_rect.width.saturating_sub(2),
            list_rect.height.saturating_sub(2),
        );
        let rows = Rect::new(
            inside.x,
            inside.y + 1,
            inside.width,
            inside.height.saturating_sub(1),
        );
        self.list_rows = rows;
        self.column_header = Rect::new(inside.x, inside.y, inside.width, 1);
        self.page_rows = usize::from(rows.height).max(1);
        self.keep_visible();
        let mut labels = String::with_capacity(48);
        for (index, column) in FeatureColumn::ALL.into_iter().enumerate() {
            if index > 0 {
                labels.push(' ');
            }
            let _ = write!(
                labels,
                "{:<width$}",
                column.label(),
                width = column.width() as usize
            );
        }
        frame.render_widget(
            Paragraph::new(labels).style(Style::default().fg(Color::Cyan)),
            self.column_header,
        );
        let items = self
            .matches
            .iter()
            .skip(self.first)
            .take(self.page_rows)
            .map(|&index| {
                let row = &self.list.rows[index];
                let metrics = self.measurements[index];
                let rt = if minutes {
                    row.rt_seconds / 60.0
                } else {
                    row.rt_seconds
                };
                let mut text = String::with_capacity(55);
                let _ = write!(text, "{:<4} {:>9.4} {:>5.1}", row.id, row.mz, rt);
                if let Some(height) = metrics.height {
                    let _ = write!(text, " {height:>7.2e}");
                } else {
                    text.push_str("       -");
                }
                if let Some(area) = metrics.area {
                    let _ = write!(text, " {area:>7.2e}");
                } else {
                    text.push_str("       -");
                }
                if let Some(isotopes) = metrics.isotopes {
                    let _ = write!(text, " {isotopes:>3}");
                } else {
                    text.push_str("   -");
                }
                if let Some(score) = metrics.score {
                    let _ = write!(text, " {score:>5.1}");
                } else {
                    text.push_str("     -");
                }
                ListItem::new(text)
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        if !self.matches.is_empty() {
            state.select(Some(self.cursor - self.first));
        }
        frame.render_stateful_widget(
            List::new(items).highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan)),
            rows,
            &mut state,
        );

        self.detail_rect = details;
        if let Some(row) = self.selected() {
            let feature = &data.features[row.feature_index];
            let mut detail = format!("Selected MS2 scan: {}\n", feature.ms2_scan_id);
            for (name, value) in self.list.headers.iter().zip(&row.values) {
                detail.push_str(name);
                detail.push_str(": ");
                detail.push_str(if value.is_empty() {
                    "(not measured)"
                } else {
                    value
                });
                detail.push('\n');
            }
            frame.render_widget(
                Paragraph::new(detail)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .title(format!(
                                "Feature {} | RT {:.3} {}",
                                row.id,
                                if minutes {
                                    row.rt_seconds / 60.0
                                } else {
                                    row.rt_seconds
                                },
                                if minutes { "min" } else { "s" },
                            )),
                    )
                    .wrap(Wrap { trim: false })
                    .scroll((self.detail_scroll, 0)),
                details,
            );
        } else {
            frame.render_widget(
                Paragraph::new("No matching accepted feature").block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .title("Details"),
                ),
                details,
            );
        }
        let hint = if self.editing {
            format!("Filter > {}█\nEnter apply  Esc stop", self.query)
        } else {
            "f/Esc  / search  click header to sort\n↑↓ rows  c clear  Enter/x XIC  s MS2".to_owned()
        };
        frame.render_widget(Paragraph::new(hint), footer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{explore_data::PickedFeature, feature_list_view::FeatureRow};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn sortable_headers_order_numeric_metrics_and_keep_missing_values_last() {
        let headers = [
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
        ]
        .map(str::to_owned)
        .to_vec();
        let rows = [
            (2, "100", "3", "10", "10"),
            (1, "9", "20", "2", "8"),
            (3, "", "", "", ""),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (id, height, area, isotopes, score))| FeatureRow {
            id,
            mz: (id * 100) as f64,
            rt_seconds: (id * 60) as f64,
            feature_index: index,
            values: [
                id.to_string(),
                (id * 100).to_string(),
                (id * 60).to_string(),
                height.into(),
                area.into(),
                isotopes.into(),
                score.into(),
            ]
            .to_vec(),
        })
        .collect();
        let mut panel = FeaturePanel::new(FeatureList { headers, rows });
        let data = Dataset {
            points: Vec::new(),
            features: [2, 1, 3]
                .into_iter()
                .map(|id| PickedFeature {
                    id,
                    mz: (id * 100) as f64,
                    rt_seconds: (id * 60) as f64,
                    apex_intensity: 1.0,
                    ms2_scan_id: format!("scan={id}"),
                })
                .collect(),
        };
        let mut terminal = Terminal::new(TestBackend::new(49, 30)).unwrap();
        let draw = |terminal: &mut Terminal<TestBackend>, panel: &mut FeaturePanel| {
            terminal
                .draw(|frame| panel.render(frame, Rect::new(0, 0, 49, 30), &data, true))
                .unwrap();
        };
        draw(&mut terminal, &mut panel);
        let table = terminal.backend().buffer();
        let header: String = (0..49).map(|x| table[(x, 3)].symbol()).collect();
        let first: String = (0..49).map(|x| table[(x, 4)].symbol()).collect();
        assert!(header.contains("Height") && header.contains("Area") && header.contains("Iso"));
        assert!(first.contains("9.00e0") && first.contains("2.00e1"));
        assert_eq!(panel.selected().unwrap().id, 1);

        panel.click(23, 3); // Height ascending; 9 before 100, not lexicographic.
        assert_eq!(panel.selected().unwrap().id, 1);
        panel.click(23, 3); // Height descending; absent stays last.
        assert_eq!(panel.selected().unwrap().id, 2);
        assert_eq!(
            panel.matches.last().map(|&row| panel.list.rows[row].id),
            Some(3)
        );
        panel.click(31, 3); // Area ascending.
        assert_eq!(panel.selected().unwrap().id, 2);
        panel.click(39, 3); // Isotope count ascending.
        assert_eq!(panel.selected().unwrap().id, 1);
        panel.click(39, 3); // Isotope count descending, 10 before 2.
        assert_eq!(panel.selected().unwrap().id, 2);

        panel.query = "isotope_peaks:2".into();
        panel.refresh();
        assert_eq!(panel.selected().unwrap().id, 1);
        assert_eq!(panel.matches.len(), 1);
        panel.click(31, 3); // Sorting does not discard the active search.
        assert_eq!(panel.matches.len(), 1);
        assert_eq!(panel.selected().unwrap().id, 1);
    }
}
