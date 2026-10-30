//! Event-driven chromatogram and accepted-feature viewer.

use std::{
    io::{self, Stdout},
    path::Path,
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    symbols::Marker,
    widgets::{
        Block, Borders, Paragraph,
        canvas::{Canvas, Line, Points},
    },
};

use crate::{
    explore_data::{self, Dataset},
    inspect::Chromatogram,
    spectrum::Peak,
};

struct TerminalSession;

impl TerminalSession {
    fn start() -> Result<Self> {
        terminal::enable_raw_mode().context("enabling terminal raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, Hide) {
            // The command may have partially succeeded; restore every setting.
            let _ = execute!(
                io::stdout(),
                Show,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = terminal::disable_raw_mode();
            return Err(error).context("entering interactive terminal");
        }
        Ok(Self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            Show,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Clone, Copy)]
struct Window {
    start: f64,
    end: f64,
}

impl Window {
    fn at(&self, x: u16, rect: Rect) -> f64 {
        if rect.width <= 1 {
            return self.start;
        }
        self.start
            + (self.end - self.start) * f64::from(x.saturating_sub(rect.x))
                / f64::from(rect.width - 1)
    }

    fn column(&self, rt: f64, rect: Rect) -> Option<u16> {
        if rect.width == 0 || !rt.is_finite() || rt < self.start || rt > self.end {
            return None;
        }
        let fraction = ((rt - self.start) / (self.end - self.start)).clamp(0.0, 1.0);
        Some(rect.x + (fraction * f64::from(rect.width - 1)).round() as u16)
    }

    fn zoom(&mut self, full: Window, anchor: f64, factor: f64) {
        let span = self.end - self.start;
        let full_span = full.end - full.start;
        let min_span =
            (full_span * 1e-9).max(f64::EPSILON * full.end.abs().max(full.start.abs()) * 32.0);
        let width = (span * factor).clamp(min_span.min(full_span), full_span);
        let fraction = ((anchor - self.start) / span).clamp(0.0, 1.0);
        let start = (anchor - fraction * width).clamp(full.start, full.end - width);
        self.start = start;
        self.end = start + width;
    }
}

fn full_window(data: &Dataset) -> Window {
    let (mut start, mut end) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in &data.points {
        if point.rt_seconds.is_finite() {
            start = start.min(point.rt_seconds);
            end = end.max(point.rt_seconds);
        }
    }
    if !start.is_finite() || !end.is_finite() {
        return Window {
            start: 0.0,
            end: 1.0,
        };
    }
    if start == end {
        let pad = (start.abs() * 1e-6).max(1e-6);
        return Window {
            start: start - pad,
            end: end + pad,
        };
    }
    Window { start, end }
}

fn plot_rect(outer: Rect) -> Rect {
    Rect {
        x: outer.x.saturating_add(2),
        y: outer.y.saturating_add(1),
        width: outer.width.saturating_sub(3),
        height: outer.height.saturating_sub(3),
    }
}

fn intensity_at(data: &Dataset, rt: f64) -> Option<f64> {
    let points = &data.points;
    if points.is_empty() || rt < points.first()?.rt_seconds || rt > points.last()?.rt_seconds {
        return None;
    }
    let index = points.partition_point(|point| point.rt_seconds < rt);
    if index == points.len() {
        return None;
    }
    let upper = &points[index];
    if index == 0 || upper.rt_seconds == rt {
        return upper.intensity.is_finite().then_some(upper.intensity);
    }
    let lower = &points[index - 1];
    let width = upper.rt_seconds - lower.rt_seconds;
    if width <= 0.0 {
        return None;
    }
    let y = lower.intensity + (upper.intensity - lower.intensity) * (rt - lower.rt_seconds) / width;
    y.is_finite().then_some(y)
}

fn y_scale(data: &Dataset, window: Window) -> f64 {
    let mut max: f64 = 0.0;
    for rt in [window.start, window.end] {
        if let Some(y) = intensity_at(data, rt) {
            max = max.max(y);
        }
    }
    let start = data
        .points
        .partition_point(|point| point.rt_seconds < window.start);
    let end = data
        .points
        .partition_point(|point| point.rt_seconds <= window.end);
    for point in &data.points[start..end] {
        max = max.max(point.intensity);
    }
    if max <= 0.0 { 1.0 } else { max }
}

fn row(y: f64, max: f64, rect: Rect) -> u16 {
    let fraction = (y / max).clamp(0.0, 1.0);
    rect.y + ((1.0 - fraction) * f64::from(rect.height - 1)).round() as u16
}

fn paint(buffer: &mut Buffer, x: u16, y: u16, symbol: &'static str, color: Color) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_symbol(symbol)
            .set_style(Style::default().fg(color));
    }
}

fn chrom_line(
    data: &Dataset,
    window: Window,
    rect: Rect,
    max: f64,
    frame: &mut ratatui::Frame<'_>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let points = &data.points;
    let start = points
        .partition_point(|point| point.rt_seconds < window.start)
        .saturating_sub(1);
    let end = points
        .partition_point(|point| point.rt_seconds <= window.end)
        .min(points.len() - 1);
    frame.render_widget(
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([window.start, window.end])
            .y_bounds([0.0, max])
            .paint(|ctx| {
                for pair in points[start..=end].windows(2) {
                    let (a, b) = (&pair[0], &pair[1]);
                    if a.rt_seconds > window.end || b.rt_seconds < window.start {
                        continue;
                    }
                    let left = a.rt_seconds.max(window.start);
                    let right = b.rt_seconds.min(window.end);
                    if b.rt_seconds == a.rt_seconds {
                        ctx.draw(&Points {
                            coords: &[(left, a.intensity.max(b.intensity))],
                            color: Color::Cyan,
                        });
                        continue;
                    }
                    let at = |rt: f64| {
                        a.intensity
                            + (b.intensity - a.intensity) * (rt - a.rt_seconds)
                                / (b.rt_seconds - a.rt_seconds)
                    };
                    ctx.draw(&Line {
                        x1: left,
                        y1: at(left),
                        x2: right,
                        y2: at(right),
                        color: Color::Cyan,
                    });
                }
                for point in &points[start..=end] {
                    if point.rt_seconds >= window.start && point.rt_seconds <= window.end {
                        ctx.draw(&Points {
                            coords: &[(point.rt_seconds, point.intensity)],
                            color: Color::Cyan,
                        });
                    }
                }
            }),
        rect,
    );
}

fn marker(
    data: &Dataset,
    index: usize,
    window: Window,
    rect: Rect,
    max: f64,
) -> Option<(u16, u16)> {
    let feature = &data.features[index];
    let x = window.column(feature.rt_seconds, rect)?;
    let y = intensity_at(data, feature.rt_seconds)?;
    Some((x, row(y, max, rect)))
}

fn spectrum_limits(peaks: &[Peak]) -> Option<(f64, f64, f64)> {
    let (mut min_mz, mut max_mz, mut max_intensity) = (f64::INFINITY, f64::NEG_INFINITY, 0.0_f64);
    for peak in peaks {
        if peak.mz.is_finite() && peak.intensity.is_finite() && peak.intensity >= 0.0 {
            min_mz = min_mz.min(peak.mz);
            max_mz = max_mz.max(peak.mz);
            max_intensity = max_intensity.max(peak.intensity);
        }
    }
    min_mz
        .is_finite()
        .then_some((min_mz, max_mz, max_intensity))
}

fn spectrum_plot(
    peaks: &[Peak],
    rect: Rect,
    (min_mz, max_mz, max_intensity): (f64, f64, f64),
    buffer: &mut Buffer,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let span = max_mz - min_mz;
    let scale = if max_intensity > 0.0 {
        max_intensity
    } else {
        1.0
    };
    let baseline = rect.y + rect.height - 1;
    for peak in peaks {
        if !peak.mz.is_finite() || !peak.intensity.is_finite() || peak.intensity < 0.0 {
            continue;
        }
        let fraction = if span > 0.0 {
            ((peak.mz - min_mz) / span).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let x = rect.x + (fraction * f64::from(rect.width - 1)).round() as u16;
        let top = row(peak.intensity, scale, rect);
        for y in top..=baseline {
            paint(buffer, x, y, "│", Color::Green);
        }
    }
}

struct Viewer<'a> {
    raw: &'a Path,
    data: Dataset,
    order: Vec<usize>,
    name: String,
    full: Window,
    window: Window,
    selected: Option<usize>,
    spectrum: Option<std::result::Result<Vec<Peak>, String>>,
    status: String,
    chart: Rect,
}

impl Viewer<'_> {
    fn select(&mut self, index: usize) {
        if self.selected == Some(index) {
            self.status = format!("Selected accepted feature {}", self.data.features[index].id);
            return;
        }
        self.selected = Some(index);
        let scan = &self.data.features[index].ms2_scan_id;
        self.spectrum =
            Some(explore_data::ms2(self.raw, scan).map_err(|error| format!("{error:#}")));
        self.status = format!("Selected accepted feature {}", self.data.features[index].id);
    }

    fn cycle(&mut self, reverse: bool) {
        let rect = plot_rect(self.chart);
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let max = y_scale(&self.data, self.window);
        let len = self.data.features.len();
        if len == 0 {
            self.status = "No accepted features in this chromatogram".into();
            return;
        }
        let start = self
            .selected
            .and_then(|selected| self.order.iter().position(|&index| index == selected))
            .unwrap_or(if reverse { 0 } else { len - 1 });
        for offset in 1..=len {
            let position = if reverse {
                (start + len - offset) % len
            } else {
                (start + offset) % len
            };
            let index = self.order[position];
            if marker(&self.data, index, self.window, rect, max).is_some() {
                self.select(index);
                return;
            }
        }
        self.status = "No accepted feature marker in the visible RT range".into();
    }

    fn click(&mut self, x: u16, y: u16) {
        let rect = plot_rect(self.chart);
        if x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height {
            return;
        }
        let max = y_scale(&self.data, self.window);
        let mut best: Option<(usize, u16)> = None;
        let selected_marker = self
            .selected
            .and_then(|index| marker(&self.data, index, self.window, rect, max));
        let mut first_at_same_spot = None;
        let mut next_at_same_spot = None;
        let mut passed_selected = false;
        for &index in &self.order {
            let Some((mx, my)) = marker(&self.data, index, self.window, rect, max) else {
                continue;
            };
            let dx = mx.abs_diff(x);
            let dy = my.abs_diff(y);
            if dx > 1 || dy > 1 {
                continue;
            }
            let distance = dx + dy;
            if best.is_none_or(|(_, previous)| distance < previous) {
                best = Some((index, distance));
            }
            if selected_marker == Some((mx, my)) {
                if Some(index) == self.selected {
                    passed_selected = true;
                } else if passed_selected {
                    next_at_same_spot.get_or_insert(index);
                } else {
                    first_at_same_spot.get_or_insert(index);
                }
            }
        }
        if let Some((index, distance)) = best {
            let selected_is_nearest =
                selected_marker.is_some_and(|(mx, my)| mx.abs_diff(x) + my.abs_diff(y) == distance);
            let target = if selected_is_nearest {
                next_at_same_spot.or(first_at_same_spot).unwrap_or(index)
            } else {
                index
            };
            self.select(target);
        } else {
            self.selected = None;
            self.spectrum = None;
            self.status =
                "No accepted feature marker near click; the MS1 trace has no selected MS2 scan"
                    .into();
        }
    }

    fn render(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        if area.width < 30 || area.height < 12 {
            self.chart = Rect::default();
            frame.render_widget(
                Paragraph::new(
                    "Terminal too small (need at least 30×12). Resize or press q to quit.",
                ),
                area,
            );
            return;
        }
        let plot_height = ((area.height - 5) * 3 / 5).max(5);
        let header = Rect::new(area.x, area.y, area.width, 2);
        self.chart = Rect::new(area.x, area.y + 2, area.width, plot_height);
        let spectrum = Rect::new(
            area.x,
            self.chart.y + plot_height,
            area.width,
            area.height - 4 - plot_height,
        );
        let footer = Rect::new(area.x, area.y + area.height - 2, area.width, 2);

        frame.render_widget(
            Paragraph::new(format!(
                "{} | RT {:.3}–{:.3} s | {} accepted features",
                self.name,
                self.window.start,
                self.window.end,
                self.data.features.len()
            )),
            header,
        );
        let max = y_scale(&self.data, self.window);
        frame.render_widget(
            Block::default().borders(Borders::ALL).title(format!(
                "MS1 intensity ≤ {:.3e}  • chromatogram   ◆ accepted  ★ selected",
                max
            )),
            self.chart,
        );
        let rect = plot_rect(self.chart);
        chrom_line(&self.data, self.window, rect, max, frame);
        {
            let buffer = frame.buffer_mut();
            for index in 0..self.data.features.len() {
                if self.selected == Some(index) {
                    continue;
                }
                if let Some((x, y)) = marker(&self.data, index, self.window, rect, max) {
                    paint(buffer, x, y, "◆", Color::Magenta);
                }
            }
            if let Some(index) = self.selected {
                if let Some((x, y)) = marker(&self.data, index, self.window, rect, max) {
                    paint(buffer, x, y, "★", Color::Yellow);
                }
            }
        }
        let label = self.selected.map_or_else(
            || "MS2 spectrum — click an accepted feature marker".to_owned(),
            |index| {
                let feature = &self.data.features[index];
                format!(
                    "MS2 feature {} | m/z {:.5} | scan {} | RT {:.3} s | apex {:.3e}",
                    feature.id,
                    feature.mz,
                    feature.ms2_scan_id,
                    feature.rt_seconds,
                    feature.apex_intensity
                )
            },
        );
        frame.render_widget(
            Block::default().borders(Borders::ALL).title(label),
            spectrum,
        );
        let spec_rect = Rect::new(
            spectrum.x + 1,
            spectrum.y + 1,
            spectrum.width.saturating_sub(2),
            spectrum.height.saturating_sub(2),
        );
        match &self.spectrum {
            Some(Ok(peaks)) => {
                if let Some(limits @ (min_mz, max_mz, max_intensity)) = spectrum_limits(peaks) {
                    let bars = if spec_rect.height >= 3 {
                        frame.render_widget(
                            Paragraph::new(format!(
                                "fragment m/z {:.3}–{:.3} | max intensity {:.3e}",
                                min_mz, max_mz, max_intensity
                            )),
                            Rect::new(spec_rect.x, spec_rect.y, spec_rect.width, 1),
                        );
                        Rect::new(
                            spec_rect.x,
                            spec_rect.y + 1,
                            spec_rect.width,
                            spec_rect.height - 1,
                        )
                    } else {
                        spec_rect
                    };
                    spectrum_plot(peaks, bars, limits, frame.buffer_mut());
                }
            }
            Some(Err(error)) => frame.render_widget(
                Paragraph::new(format!("Cannot display selected MS2 scan: {error}")),
                spec_rect,
            ),
            None => frame.render_widget(
                Paragraph::new("No accepted feature selected. Click a ◆ on the chromatogram."),
                spec_rect,
            ),
        }
        frame.render_widget(
            Paragraph::new(format!(
                "q quit  +/- zoom  wheel zoom  r reset  click ◆  Tab/Shift-Tab next/prev\n{}",
                self.status
            )),
            footer,
        );
    }
}

/// Open one raw chromatogram with accepted audit features and their selected MS2 scans.
pub fn run(raw: &Path, audit: &Path, mode: Chromatogram) -> Result<()> {
    let name = match &mode {
        Chromatogram::Tic => "TIC".to_owned(),
        Chromatogram::Bpi => "BPI".to_owned(),
        Chromatogram::Xic { mz, .. } => format!("XIC m/z {mz:.5}"),
    };
    let data = explore_data::load(raw, audit, mode)?;
    let mut order: Vec<usize> = (0..data.features.len()).collect();
    order.sort_by(|&a, &b| {
        data.features[a]
            .rt_seconds
            .total_cmp(&data.features[b].rt_seconds)
            .then_with(|| data.features[a].id.cmp(&data.features[b].id))
    });
    let full = full_window(&data);
    let mut viewer = Viewer {
        raw,
        data,
        order,
        name,
        full,
        window: full,
        selected: None,
        spectrum: None,
        status: "Select an accepted feature marker to view its chosen MS2 scan".into(),
        chart: Rect::default(),
    };
    let _session = TerminalSession::start()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("initializing terminal viewer")?;
    event_loop(&mut terminal, &mut viewer)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    viewer: &mut Viewer<'_>,
) -> Result<()> {
    loop {
        terminal
            .draw(|frame| viewer.render(frame))
            .context("drawing interactive viewer")?;
        match event::read().context("reading terminal event")? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('+') | KeyCode::Char('=') => viewer.window.zoom(
                        viewer.full,
                        (viewer.window.start + viewer.window.end) / 2.0,
                        1.0 / 1.5,
                    ),
                    KeyCode::Char('-') | KeyCode::Char('_') => viewer.window.zoom(
                        viewer.full,
                        (viewer.window.start + viewer.window.end) / 2.0,
                        1.5,
                    ),
                    KeyCode::Char('r') => viewer.window = viewer.full,
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        viewer.cycle(true)
                    }
                    KeyCode::Tab | KeyCode::Right => viewer.cycle(false),
                    KeyCode::BackTab | KeyCode::Left => viewer.cycle(true),
                    _ => {}
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => viewer.click(mouse.column, mouse.row),
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let rect = plot_rect(viewer.chart);
                    let anchor = if rect.width > 0
                        && mouse.column >= rect.x
                        && mouse.column < rect.x + rect.width
                    {
                        viewer.window.at(mouse.column, rect)
                    } else {
                        (viewer.window.start + viewer.window.end) / 2.0
                    };
                    viewer.window.zoom(
                        viewer.full,
                        anchor,
                        if mouse.kind == MouseEventKind::ScrollUp {
                            1.0 / 1.5
                        } else {
                            1.5
                        },
                    );
                }
                _ => {}
            },
            _ => {}
        }
    }
}
