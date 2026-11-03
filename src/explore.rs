//! Event-driven chromatogram and accepted-feature viewer.

use std::{
    io::{self, Stdout},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
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
        Block, BorderType, Borders, Paragraph,
        canvas::{Canvas, Line, Points},
    },
};

use crate::{
    explore_data::{self, Dataset},
    explore_features::{FeaturePanel, PanelAction},
    explore_file_panel::{FileAction, FileLoadState, FilePanel, FileRow, file_color},
    explore_files::{self, MsFile},
    explore_loading::{self, FileLoader, FileRequest, LoadOperation, LoadedFile, LoadedResult},
    inspect::{self, Chromatogram},
    spectrum::Peak,
    spectrum_isotopes::{IsotopeMatch, isotope_matches},
    spectrum_plot::{self, BarStyle, SpectrumTone, SpectrumView, peak_column, visible_peak},
};

// Relative to the base peak in the visible m/z window; zero restores all nonzero peaks.
const SPECTRUM_CUTOFFS: [f64; 5] = [0.0, 0.01, 0.05, 0.10, 0.25];
const DEFAULT_SPECTRUM_CUTOFF: usize = 2;

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
    color: Color,
    frame: &mut ratatui::Frame<'_>,
) {
    if rect.width == 0 || rect.height == 0 || data.points.is_empty() {
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
                let visible = &points[start..=end];
                if visible.len() > usize::from(rect.width) * 8 {
                    // Preserve narrow spikes without drawing every raw scan on each keystroke.
                    let buckets = usize::from(rect.width) * 2;
                    let step = visible.len().div_ceil(buckets);
                    let mut previous = None;
                    for chunk in visible.chunks(step) {
                        let first = &chunk[0];
                        let last = &chunk[chunk.len() - 1];
                        let x = (first.rt_seconds + last.rt_seconds) / 2.0;
                        let middle = (first.intensity + last.intensity) / 2.0;
                        let (mut low, mut high) = (f64::INFINITY, 0.0_f64);
                        for point in chunk {
                            if point.rt_seconds >= window.start && point.rt_seconds <= window.end {
                                low = low.min(point.intensity);
                                high = high.max(point.intensity);
                            }
                        }
                        if low.is_finite() {
                            if let Some((old_x, old_y)) = previous {
                                ctx.draw(&Line {
                                    x1: old_x,
                                    y1: old_y,
                                    x2: x,
                                    y2: middle,
                                    color,
                                });
                            }
                            ctx.draw(&Line {
                                x1: x,
                                y1: low,
                                x2: x,
                                y2: high,
                                color,
                            });
                            previous = Some((x, middle));
                        }
                    }
                    return;
                }
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
                            color,
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
                        color,
                    });
                }
                for point in &points[start..=end] {
                    if point.rt_seconds >= window.start && point.rt_seconds <= window.end {
                        ctx.draw(&Points {
                            coords: &[(point.rt_seconds, point.intensity)],
                            color,
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
    xic: Option<(f64, f64)>,
) -> Option<(u16, u16)> {
    let feature = &data.features[index];
    if xic.is_some_and(|(mz, tolerance)| (feature.mz - mz).abs() > tolerance) {
        return None;
    }
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

fn mass_window(peaks: &[Peak]) -> Option<Window> {
    let (start, end, _) = spectrum_limits(peaks)?;
    if start == end {
        let pad = (start.abs() * 1e-6).max(1e-6);
        Some(Window {
            start: start - pad,
            end: end + pad,
        })
    } else {
        Some(Window { start, end })
    }
}

fn mass_limits(peaks: &[Peak], window: Window) -> (f64, f64, f64) {
    let max = peaks
        .iter()
        .filter(|peak| {
            peak.mz >= window.start && peak.mz <= window.end && peak.intensity.is_finite()
        })
        .fold(0.0_f64, |max, peak| max.max(peak.intensity));
    (window.start, window.end, max)
}

fn mirror_rects(bars: Rect) -> Option<(Rect, Rect, u16)> {
    if bars.height < 3 || bars.width == 0 {
        return None;
    }
    let upper = (bars.height - 1) / 2;
    let center = bars.y + upper;
    Some((
        Rect::new(bars.x, bars.y, bars.width, upper),
        Rect::new(bars.x, center + 1, bars.width, bars.height - upper - 1),
        center,
    ))
}

fn spectrum_labels(
    peaks: &[Peak],
    rect: Rect,
    min_mz: f64,
    max_mz: f64,
    floor: f64,
    buffer: &mut Buffer,
) {
    if rect.width < 12 {
        return;
    }
    let mut top: [Option<Peak>; 6] = [None; 6];
    for &peak in peaks {
        if !visible_peak(&peak, min_mz, max_mz, floor) {
            continue;
        }
        for index in 0..top.len() {
            if top[index].is_none_or(|other| peak.intensity > other.intensity) {
                top[index..].rotate_right(1);
                top[index] = Some(peak);
                break;
            }
        }
    }
    let mut placed = [None; 6];
    for peak in top.into_iter().flatten() {
        let x = peak_column(peak.mz, rect, min_mz, max_mz);
        if placed
            .iter()
            .flatten()
            .any(|&previous: &u16| previous.abs_diff(x) < 9)
        {
            continue;
        }
        let text = format!("{:.2}", peak.mz);
        if text.len() > usize::from(rect.width) {
            continue;
        }
        let left = x
            .saturating_sub((text.len() / 2) as u16)
            .clamp(rect.x, rect.right().saturating_sub(text.len() as u16));
        buffer.set_string(left, rect.y, text, Style::default().fg(Color::Green));
        if let Some(slot) = placed.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(x);
        }
    }
}

fn horizontal_axis(buffer: &mut Buffer, rect: Rect, start: f64, end: f64, unit: &str) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let span = end - start;
    let decimals = if span >= 10.0 {
        2
    } else if span >= 1.0 {
        3
    } else if span >= 0.1 {
        4
    } else if span >= 0.01 {
        5
    } else {
        6
    };
    for x in rect.x..rect.right() {
        paint(buffer, x, rect.y, "─", Color::DarkGray);
    }
    let first = format!("{start:.decimals$}");
    let last = format!("{end:.decimals$}");
    let width = usize::from(rect.width);
    if first.len() + last.len() + 2 > width {
        buffer.set_string(rect.x, rect.y, unit, Style::default().fg(Color::Gray));
        return;
    }
    let last_x = rect.right() - last.len() as u16;
    let style = Style::default().fg(Color::Gray);
    buffer.set_string(rect.x, rect.y, &first, style);
    buffer.set_string(last_x, rect.y, &last, style);
    let middle = format!("{:.decimals$} {unit}", (start + end) / 2.0);
    let middle = if middle.len() + first.len() + last.len() + 4 <= width {
        middle.as_str()
    } else {
        unit
    };
    let center = rect.x + rect.width / 2;
    let middle_x = center.saturating_sub((middle.len() / 2) as u16);
    if middle_x > rect.x + first.len() as u16 && middle_x + (middle.len() as u16) < last_x {
        buffer.set_string(middle_x, rect.y, middle, style);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MassLevel {
    Ms1,
    Ms2,
}

struct SpectrumRequest {
    generation: u64,
    rt: f64,
    ms2_id: Option<String>,
}

struct SpectrumReply {
    generation: u64,
    result: std::result::Result<inspect::SelectedSpectra, String>,
}

struct TraceRequest {
    generation: u64,
    mode: Chromatogram,
}

struct TraceReply {
    generation: u64,
    mode: Chromatogram,
    result: std::result::Result<Vec<explore_data::ChromPoint>, String>,
}

struct Background {
    spectrum_tx: Sender<SpectrumRequest>,
    spectrum_rx: Receiver<SpectrumReply>,
    trace_tx: Sender<TraceRequest>,
    trace_rx: Receiver<TraceReply>,
}

impl Background {
    fn start(raw: &Path) -> Self {
        let (spectrum_tx, requests) = mpsc::channel::<SpectrumRequest>();
        let (replies, spectrum_rx) = mpsc::channel();
        let spectra_path = PathBuf::from(raw);
        thread::spawn(move || {
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let result =
                    inspect::spectra_at(&spectra_path, request.rt, request.ms2_id.as_deref())
                        .map_err(|error| format!("{error:#}"));
                if replies
                    .send(SpectrumReply {
                        generation: request.generation,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let (trace_tx, requests) = mpsc::channel::<TraceRequest>();
        let (replies, trace_rx) = mpsc::channel();
        let trace_path = PathBuf::from(raw);
        thread::spawn(move || {
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let result = explore_data::chromatogram(&trace_path, request.mode)
                    .map_err(|error| format!("{error:#}"));
                if replies
                    .send(TraceReply {
                        generation: request.generation,
                        mode: request.mode,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            spectrum_tx,
            spectrum_rx,
            trace_tx,
            trace_rx,
        }
    }
}

struct PinnedMs2 {
    feature_id: usize,
    scan_id: String,
    spectra: Arc<inspect::SelectedSpectra>,
    full: Window,
    isotopes: Option<Arc<Vec<Option<IsotopeMatch>>>>,
}

struct FileState {
    cached: Option<LoadedFile>,
    enabled: bool,
    generation: u64,
    queued: Option<u64>,
    error: Option<String>,
}

struct Viewer {
    data: Dataset,
    catalog: Arc<[MsFile]>,
    files: Vec<FileState>,
    active_file: usize,
    pending_file: Option<usize>,
    file_panel: Option<FilePanel>,
    loader: FileLoader,
    mode: Chromatogram,
    full_mode: Chromatogram,
    chrom_generation: u64,
    order: Vec<usize>,
    name: String,
    full: Window,
    window: Window,
    rt_minutes: bool,
    selected: Option<usize>,
    selected_rt: Option<f64>,
    level: MassLevel,
    spectrum_cutoff: usize,
    bar_style: BarStyle,
    isotope_highlight: bool,
    ms1_isotopes: Option<Vec<Option<IsotopeMatch>>>,
    ms2_isotopes: Option<Arc<Vec<Option<IsotopeMatch>>>>,
    pinned_ms2: Option<PinnedMs2>,
    spectrum: Option<std::result::Result<Arc<inspect::SelectedSpectra>, String>>,
    spectrum_window: Option<Window>,
    focused_peak: Option<Peak>,
    focused_reference: bool,
    background: Background,
    spectrum_generation: u64,
    trace_generation: u64,
    pending_spectrum: bool,
    pending_trace: Option<Chromatogram>,
    xic: Option<(f64, f64)>,
    ppm: f64,
    absolute_mz: f64,
    status: String,
    mass_search: Option<String>,
    feature_panel: Option<FeaturePanel>,
    chart: Rect,
    spectrum_bars: Rect,
}

impl Viewer {
    fn display_rt(&self, seconds: f64) -> f64 {
        if self.rt_minutes {
            seconds / 60.0
        } else {
            seconds
        }
    }

    fn rt_unit(&self) -> &'static str {
        if self.rt_minutes { "min" } else { "s" }
    }

    fn file_data(&self, index: usize) -> Option<&Dataset> {
        let state = self.files.get(index)?;
        if !state.enabled || state.generation != self.chrom_generation {
            return None;
        }
        if index == self.active_file {
            Some(&self.data)
        } else {
            state.cached.as_ref().map(|loaded| &loaded.data)
        }
    }

    fn chart_scale(&self) -> f64 {
        (0..self.catalog.len())
            .filter_map(|index| self.file_data(index))
            .fold(0.0_f64, |max, data| max.max(y_scale(data, self.window)))
            .max(1.0)
    }

    fn reframe(&mut self, reset: bool) {
        let mut full: Option<Window> = None;
        for index in 0..self.files.len() {
            if let Some(data) = self.file_data(index) {
                if !data.points.is_empty() {
                    let range = full_window(data);
                    full = Some(full.map_or(range, |old| Window {
                        start: old.start.min(range.start),
                        end: old.end.max(range.end),
                    }));
                }
            }
        }
        self.full = full.unwrap_or_else(|| full_window(&self.data));
        if reset {
            self.window = self.full;
        } else {
            self.window.start = self.window.start.max(self.full.start);
            self.window.end = self.window.end.min(self.full.end);
            if self.window.start >= self.window.end {
                self.window = self.full;
            }
        }
    }

    fn queue_file(&mut self, index: usize) {
        if index == self.active_file || self.files[index].queued == Some(self.chrom_generation) {
            return;
        }
        let operation = if self.files[index].cached.is_some() {
            LoadOperation::Chromatogram(self.mode)
        } else {
            LoadOperation::Dataset(self.mode)
        };
        if self.loader.request(FileRequest {
            index,
            generation: self.chrom_generation,
            operation,
        }) {
            self.files[index].queued = Some(self.chrom_generation);
            self.files[index].error = None;
        } else {
            self.status = "MS file loader stopped unexpectedly".into();
        }
    }

    fn activate_file(&mut self, index: usize) {
        let Some(next) = self.files[index].cached.take() else {
            return;
        };
        let old = self.active_file;
        let feature_list_was_visible = self.feature_panel.as_ref().is_some_and(|p| p.visible);
        let restoring_full = self
            .pending_trace
            .is_some_and(|mode| !matches!(mode, Chromatogram::Xic { .. }));
        self.files[old].cached = Some(LoadedFile {
            data: std::mem::replace(&mut self.data, next.data),
            order: std::mem::replace(&mut self.order, next.order),
            feature_panel: std::mem::replace(&mut self.feature_panel, next.feature_panel),
        });
        self.active_file = index;
        self.files[index].enabled = true;
        self.pending_file = None;
        if feature_list_was_visible {
            if let Some(panel) = &mut self.feature_panel {
                panel.visible = true;
            } else if let Some(panel) = &mut self.file_panel {
                panel.visible = true;
            }
        }
        if let Some(panel) = &mut self.file_panel {
            panel.set_cursor(index, self.files.len());
        }
        self.background = Background::start(&self.catalog[index].raw);
        self.spectrum_generation = self.spectrum_generation.wrapping_add(1);
        self.trace_generation = self.trace_generation.wrapping_add(1);
        self.pending_spectrum = false;
        self.pending_trace = None;
        self.spectrum = None;
        self.spectrum_window = None;
        self.ms1_isotopes = None;
        self.ms2_isotopes = None;
        self.pinned_ms2 = None;
        self.focused_peak = None;
        self.focused_reference = false;
        self.selected = None;
        self.selected_rt = None;
        self.level = MassLevel::Ms1;
        self.mass_search = None;
        self.reframe(true);
        self.status = format!(
            "Viewing {}/{}: {} ({} traces visible)",
            index + 1,
            self.catalog.len(),
            self.catalog[index].relative.display(),
            self.files.iter().filter(|file| file.enabled).count()
        );
        if restoring_full {
            self.restore_full();
        }
    }

    fn open_file(&mut self, index: usize) {
        if index == self.active_file {
            self.pending_file = None;
            self.files[index].enabled = true;
            self.reframe(true);
            return;
        }
        if self.files[index].cached.is_some()
            && self.files[index].generation == self.chrom_generation
        {
            self.activate_file(index);
        } else {
            self.pending_file = Some(index);
            self.queue_file(index);
            self.status = format!(
                "Loading {}… navigate and zoom while it opens",
                self.catalog[index].relative.display()
            );
        }
    }

    fn cycle_file(&mut self, forward: bool) {
        let base = self.pending_file.unwrap_or(self.active_file);
        let index = if forward {
            (base + 1) % self.catalog.len()
        } else {
            (base + self.catalog.len() - 1) % self.catalog.len()
        };
        self.open_file(index);
    }

    fn toggle_file(&mut self, index: usize) {
        self.files[index].enabled = !self.files[index].enabled;
        if self.files[index].enabled
            && (self.files[index].generation != self.chrom_generation
                || (index != self.active_file && self.files[index].cached.is_none()))
        {
            self.queue_file(index);
        }
        self.reframe(true);
        self.status = format!(
            "{} {}",
            if self.files[index].enabled {
                "Overlay on:"
            } else {
                "Overlay off:"
            },
            self.catalog[index].relative.display()
        );
    }

    fn file_action(&mut self, action: FileAction) {
        match action {
            FileAction::Stay | FileAction::Pass => {}
            FileAction::Close => self.file_panel.as_mut().expect("file panel open").visible = false,
            FileAction::Activate(index) => self.open_file(index),
            FileAction::Toggle(index) => self.toggle_file(index),
        }
    }

    fn poll_files(&mut self) -> bool {
        let mut changed = false;
        while let Some(reply) = self.loader.poll() {
            let state = &mut self.files[reply.index];
            if state.queued != Some(reply.generation) {
                continue;
            }
            state.queued = None;
            let mut newly_loaded = false;
            match reply.result {
                Ok(LoadedResult::Dataset(loaded)) => {
                    newly_loaded = state.cached.is_none();
                    state.cached = Some(loaded);
                    state.generation = reply.generation;
                    state.error = None;
                }
                Ok(LoadedResult::Chromatogram(points)) => {
                    if let Some(loaded) = &mut state.cached {
                        loaded.data.points = points;
                        state.generation = reply.generation;
                        state.error = None;
                    }
                }
                Err(error) => {
                    state.error = Some(error.clone());
                    self.status = format!(
                        "Cannot open {}: {error}",
                        self.catalog[reply.index].relative.display()
                    );
                    if self.pending_file == Some(reply.index) {
                        self.pending_file = None;
                    }
                }
            }
            if self.pending_file == Some(reply.index)
                && self.files[reply.index].generation == self.chrom_generation
                && self.files[reply.index].cached.is_some()
            {
                self.activate_file(reply.index);
            } else if self.files[reply.index].enabled
                && self.files[reply.index].generation != self.chrom_generation
                && self.files[reply.index].error.is_none()
            {
                self.queue_file(reply.index);
            } else {
                self.reframe(newly_loaded && self.files[reply.index].enabled);
            }
            changed = true;
        }
        changed
    }

    fn request_spectrum(&mut self, rt: f64, ms2_id: Option<String>) {
        self.spectrum_generation = self.spectrum_generation.wrapping_add(1);
        self.spectrum = None;
        self.ms1_isotopes = None;
        self.ms2_isotopes = None;
        self.focused_reference = false;
        self.focused_peak = None;
        self.spectrum_window = None;
        self.pending_spectrum = self
            .background
            .spectrum_tx
            .send(SpectrumRequest {
                generation: self.spectrum_generation,
                rt,
                ms2_id,
            })
            .is_ok();
        if !self.pending_spectrum {
            self.status = "Spectrum reader stopped unexpectedly".into();
        }
    }

    fn select(&mut self, index: usize) {
        if self.selected == Some(index) {
            self.show_level(MassLevel::Ms2);
            self.status = format!("Selected accepted feature {}", self.data.features[index].id);
            return;
        }
        self.selected = Some(index);
        self.show_level(MassLevel::Ms2);
        let feature = &self.data.features[index];
        self.selected_rt = Some(feature.rt_seconds);
        let rt = feature.rt_seconds;
        let scan = feature.ms2_scan_id.clone();
        let id = feature.id;
        self.request_spectrum(rt, Some(scan));
        self.status = format!("Loading MS1 and MS2 for feature {id}…");
    }

    fn toggle_pin(&mut self) {
        if self.pinned_ms2.take().is_some() {
            self.spectrum_window = None;
            self.focused_peak = None;
            self.focused_reference = false;
            self.status = "MS2 reference unpinned".into();
            return;
        }
        if self.level != MassLevel::Ms2 {
            self.status = "Select an MS2 spectrum before pinning (2 or s in features)".into();
            return;
        }
        let Some(index) = self.selected else {
            self.status = "Select a feature with MS2 before pinning".into();
            return;
        };
        let Some(spectra) = self
            .spectrum
            .as_ref()
            .and_then(|result| result.as_ref().ok())
        else {
            self.status = "Wait for the selected MS2 scan before pinning".into();
            return;
        };
        let Some(full) = spectra.ms2.as_deref().and_then(mass_window) else {
            self.status = "Selected MS2 has no finite centroid peaks to pin".into();
            return;
        };
        let feature = &self.data.features[index];
        self.pinned_ms2 = Some(PinnedMs2 {
            feature_id: feature.id,
            scan_id: feature.ms2_scan_id.clone(),
            spectra: Arc::clone(spectra),
            full,
            isotopes: self.ms2_isotopes.as_ref().map(Arc::clone),
        });
        self.spectrum_window = None;
        self.status = format!(
            "Pinned MS2 feature {} scan {}; select another feature to compare (p unpins)",
            feature.id, feature.ms2_scan_id
        );
    }

    fn focus_feature(&mut self, rt: f64) {
        let span = (self.full.end - self.full.start).min(60.0);
        let start = (rt - span / 2.0).clamp(self.full.start, self.full.end - span);
        self.window = Window {
            start,
            end: start + span,
        };
    }

    fn panel_action(&mut self, action: PanelAction) {
        match action {
            PanelAction::Stay | PanelAction::Pass => {}
            PanelAction::Close => self.feature_panel.as_mut().expect("panel open").visible = false,
            PanelAction::Xic { index, mz, rt } => {
                self.focus_feature(rt);
                self.select(index);
                self.show_level(MassLevel::Ms1);
                self.request_xic(mz);
            }
            PanelAction::Ms2 { index, rt } => {
                self.focus_feature(rt);
                self.select(index);
            }
        }
    }

    fn poll_results(&mut self) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.background.spectrum_rx.try_recv() {
            if reply.generation == self.spectrum_generation {
                self.pending_spectrum = false;
                if let Err(error) = &reply.result {
                    self.status = format!("Cannot read selected spectra: {error}");
                } else if self.pending_trace.is_none() {
                    self.status = "Spectrum ready: click a peak to extract its MS1 XIC".into();
                }
                self.spectrum = Some(reply.result.map(Arc::new));
                changed = true;
            }
        }
        while let Ok(reply) = self.background.trace_rx.try_recv() {
            if reply.generation == self.trace_generation {
                self.pending_trace = None;
                match reply.result {
                    Ok(points) => {
                        self.data.points = points;
                        self.mode = reply.mode;
                        self.chrom_generation = self.chrom_generation.wrapping_add(1);
                        self.files[self.active_file].generation = self.chrom_generation;
                        self.reframe(!matches!(reply.mode, Chromatogram::Xic { .. }));
                        for index in 0..self.files.len() {
                            if index != self.active_file && self.files[index].enabled {
                                self.queue_file(index);
                            }
                        }
                        self.xic = inspect::xic_window(reply.mode).expect("validated trace mode");
                        match reply.mode {
                            Chromatogram::Tic => {
                                self.name = "TIC".into();
                                self.status = "Restored full TIC chromatogram".into();
                            }
                            Chromatogram::Bpi => {
                                self.name = "BPI".into();
                                self.status = "Restored full BPI chromatogram".into();
                            }
                            Chromatogram::Xic {
                                mz,
                                ppm,
                                absolute_mz,
                            } => {
                                let tolerance = (mz * ppm * 1e-6).max(absolute_mz);
                                self.name = format!("XIC m/z {mz} ± {tolerance} Da");
                                self.status = format!("Extracted MS1 XIC for m/z {mz}");
                            }
                        }
                    }
                    Err(error) => self.status = format!("Chromatogram extraction failed: {error}"),
                }
                changed = true;
            }
        }
        changed
    }

    fn visible_peaks(&self) -> Option<&[Peak]> {
        let spectra = self.spectrum.as_ref()?.as_ref().ok()?;
        match self.level {
            MassLevel::Ms1 => Some(&spectra.ms1),
            MassLevel::Ms2 => spectra.ms2.as_deref(),
        }
    }

    fn mirror_reference(&self) -> Option<&PinnedMs2> {
        let pinned = self.pinned_ms2.as_ref()?;
        let feature = self.data.features.get(self.selected?)?;
        (self.level == MassLevel::Ms2
            && feature.ms2_scan_id != pinned.scan_id
            && self.visible_peaks().is_some())
        .then_some(pinned)
    }

    fn full_spectrum_window(&self) -> Option<Window> {
        let current = self.visible_peaks().and_then(mass_window)?;
        let Some(reference) = self.mirror_reference() else {
            return Some(current);
        };
        let pinned = reference.full;
        Some(Window {
            start: current.start.min(pinned.start),
            end: current.end.max(pinned.end),
        })
    }

    fn toggle_isotopes(&mut self) {
        self.isotope_highlight = !self.isotope_highlight;
        if !self.isotope_highlight {
            self.ms1_isotopes = None;
            self.ms2_isotopes = None;
            if let Some(reference) = &mut self.pinned_ms2 {
                reference.isotopes = None;
            }
        }
        self.status = if self.isotope_highlight {
            "Isotope candidates ON (magenta; M+1..+3, z=1 spacing)".into()
        } else {
            "Isotope candidate highlighting OFF".into()
        };
    }

    fn ensure_isotopes(&mut self) {
        if !self.isotope_highlight {
            return;
        }
        if let Some(Ok(spectra)) = &self.spectrum {
            match self.level {
                MassLevel::Ms1 if self.ms1_isotopes.is_none() => {
                    self.ms1_isotopes = Some(isotope_matches(&spectra.ms1));
                }
                MassLevel::Ms2 if self.ms2_isotopes.is_none() => {
                    self.ms2_isotopes = spectra.ms2.as_ref().map(|peaks| {
                        if self
                            .pinned_ms2
                            .as_ref()
                            .is_some_and(|reference| Arc::ptr_eq(&reference.spectra, spectra))
                        {
                            if let Some(reference) = &self.pinned_ms2 {
                                if let Some(matches) = &reference.isotopes {
                                    return Arc::clone(matches);
                                }
                            }
                        }
                        Arc::new(isotope_matches(peaks))
                    });
                }
                _ => {}
            }
        }
        if self.mirror_reference().is_some()
            && let Some(reference) = &mut self.pinned_ms2
            && reference.isotopes.is_none()
        {
            reference.isotopes = reference
                .spectra
                .ms2
                .as_ref()
                .map(|peaks| Arc::new(isotope_matches(peaks)));
        }
    }

    fn show_level(&mut self, level: MassLevel) {
        if self.level != level {
            self.level = level;
            self.spectrum_window = None;
            self.focused_peak = None;
            self.focused_reference = false;
        }
    }

    fn zoom_mass(&mut self, column: u16, factor: f64) {
        let Some(full) = self.full_spectrum_window() else {
            return;
        };
        let mut window = self.spectrum_window.unwrap_or(full);
        let rect = self.spectrum_bars;
        let anchor = if column >= rect.x && column < rect.right() {
            window.at(column, rect)
        } else {
            (window.start + window.end) / 2.0
        };
        window.zoom(full, anchor, factor);
        self.spectrum_window = Some(window);
    }

    fn request_xic(&mut self, mz: f64) {
        let mode = Chromatogram::Xic {
            mz,
            ppm: self.ppm,
            absolute_mz: self.absolute_mz,
        };
        self.trace_generation = self.trace_generation.wrapping_add(1);
        self.pending_trace = self
            .background
            .trace_tx
            .send(TraceRequest {
                generation: self.trace_generation,
                mode,
            })
            .is_ok()
            .then_some(mode);
        self.status = if self.pending_trace.is_some() {
            format!("Extracting MS1 XIC at m/z {mz}… zoom and selection remain usable")
        } else {
            "Chromatogram reader stopped unexpectedly".into()
        };
    }

    fn restore_full(&mut self) {
        if self
            .pending_trace
            .is_some_and(|mode| !matches!(mode, Chromatogram::Xic { .. }))
        {
            return;
        }
        if !matches!(self.mode, Chromatogram::Xic { .. }) {
            self.trace_generation = self.trace_generation.wrapping_add(1);
            self.pending_trace = None;
            self.window = self.full;
            self.status = "Full chromatogram already displayed".into();
            return;
        }
        self.trace_generation = self.trace_generation.wrapping_add(1);
        let mode = self.full_mode;
        self.pending_trace = self
            .background
            .trace_tx
            .send(TraceRequest {
                generation: self.trace_generation,
                mode,
            })
            .is_ok()
            .then_some(mode);
        self.status = if self.pending_trace.is_some() {
            "Loading full chromatogram… zoom and selection remain usable".into()
        } else {
            "Chromatogram reader stopped unexpectedly".into()
        };
    }

    fn submit_mass_search(&mut self) {
        let text = self.mass_search.as_deref().unwrap_or_default();
        let Ok(mz) = text.parse::<f64>() else {
            self.status = "Enter a positive numeric m/z".into();
            return;
        };
        if let Err(error) = inspect::xic_window(Chromatogram::Xic {
            mz,
            ppm: self.ppm,
            absolute_mz: self.absolute_mz,
        }) {
            self.status = format!("{error:#}");
            return;
        }
        self.mass_search = None;
        self.focused_peak = None;
        self.request_xic(mz);
    }

    fn search_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                self.mass_search = None;
                self.status = "Exact-mass search cancelled".into();
            }
            KeyCode::Enter => self.submit_mass_search(),
            KeyCode::Backspace => {
                self.mass_search.as_mut().expect("search active").pop();
            }
            KeyCode::Char(c)
                if (c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E'))
                    && self.mass_search.as_ref().expect("search active").len() < 32 =>
            {
                self.mass_search.as_mut().expect("search active").push(c);
            }
            _ => {}
        }
    }

    fn click_spectrum(&mut self, x: u16, y: u16) {
        let Some(full) = self.full_spectrum_window() else {
            return;
        };
        let window = self.spectrum_window.unwrap_or(full);
        let (peaks, rect, reference, inverted) =
            match (self.mirror_reference(), mirror_rects(self.spectrum_bars)) {
                (Some(pinned), Some((upper, _, center))) if y < center => {
                    let Some(peaks) = pinned.spectra.ms2.as_deref() else {
                        return;
                    };
                    (peaks, upper, true, false)
                }
                (Some(_), Some((_, lower, center))) if y > center => {
                    let Some(peaks) = self.visible_peaks() else {
                        return;
                    };
                    (peaks, lower, false, true)
                }
                (Some(_), Some(_)) => return,
                _ => {
                    let Some(peaks) = self.visible_peaks() else {
                        return;
                    };
                    (peaks, self.spectrum_bars, false, false)
                }
            };
        let limits = mass_limits(peaks, window);
        let view = SpectrumView {
            peaks,
            limits,
            floor: limits.2 * SPECTRUM_CUTOFFS[self.spectrum_cutoff],
            isotopes: None,
            focused_mz: None,
            style: self.bar_style,
            tone: if reference {
                SpectrumTone::Reference
            } else {
                SpectrumTone::Current
            },
            inverted,
        };
        let Some(peak) = spectrum_plot::peak_at(&view, rect, x, y) else {
            return;
        };
        self.focused_peak = Some(peak);
        self.focused_reference = reference;
        self.request_xic(peak.mz);
    }

    fn select_ms1(&mut self, rt: f64) {
        let points = &self.data.points;
        let next = points.partition_point(|point| point.rt_seconds < rt);
        let index = if next == points.len() {
            points.len() - 1
        } else if next > 0 && rt - points[next - 1].rt_seconds <= points[next].rt_seconds - rt {
            next - 1
        } else {
            next
        };
        let rt = points[index].rt_seconds;
        self.selected = None;
        self.selected_rt = Some(rt);
        self.level = MassLevel::Ms1;
        self.request_spectrum(rt, None);
        self.status = format!("Loading MS1 spectrum near RT {rt:.3} s…");
    }

    fn cycle(&mut self, reverse: bool) {
        let rect = plot_rect(self.chart);
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let max = self.chart_scale();
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
            if marker(&self.data, index, self.window, rect, max, self.xic).is_some() {
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
        if !self.files[self.active_file].enabled {
            self.status = "Active trace hidden; show it with o or select a visible file".into();
            return;
        }
        let max = self.chart_scale();
        let mut best: Option<(usize, u16)> = None;
        let selected_marker = self
            .selected
            .and_then(|index| marker(&self.data, index, self.window, rect, max, self.xic));
        let mut first_at_same_spot = None;
        let mut next_at_same_spot = None;
        let mut passed_selected = false;
        for &index in &self.order {
            let Some((mx, my)) = marker(&self.data, index, self.window, rect, max, self.xic) else {
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
            self.select_ms1(self.window.at(x, rect));
        }
    }

    fn render_spectrum(
        &self,
        frame: &mut ratatui::Frame<'_>,
        spec_rect: Rect,
        peaks: &[Peak],
        full: Window,
    ) {
        let window = self.spectrum_window.unwrap_or(full);
        let limits @ (min_mz, max_mz, max_intensity) = mass_limits(peaks, window);
        let cutoff = SPECTRUM_CUTOFFS[self.spectrum_cutoff];
        let floor = max_intensity * cutoff;
        let comparison = self.mirror_reference().and_then(|pinned| {
            pinned.spectra.ms2.as_deref().and_then(|reference_peaks| {
                mirror_rects(self.spectrum_bars)
                    .map(|(upper, lower, center)| (pinned, reference_peaks, upper, lower, center))
            })
        });
        if spec_rect.height >= 2 {
            let selected = self.focused_peak.map_or_else(String::new, |peak| {
                let side = if comparison.is_some() {
                    if self.focused_reference { "↑" } else { "↓" }
                } else {
                    ""
                };
                format!(
                    " | {side}m/z {:.5} intensity {:.3e}",
                    peak.mz, peak.intensity
                )
            });
            let info = if let Some((_, reference_peaks, _, _, _)) = comparison {
                let reference_base = mass_limits(reference_peaks, window).2;
                format!(
                    "↑ref {:.2e} ↓current {:.2e} | ≥{:.0}% base{}",
                    reference_base,
                    max_intensity,
                    cutoff * 100.0,
                    selected
                )
            } else {
                format!(
                    "base {:.3e} | peaks ≥ {:.0}% base  h less  l more{}",
                    max_intensity,
                    cutoff * 100.0,
                    selected
                )
            };
            frame.render_widget(
                Paragraph::new(info),
                Rect::new(spec_rect.x, spec_rect.y, spec_rect.width, 1),
            );
        }
        let current_isotopes = if self.isotope_highlight {
            match self.level {
                MassLevel::Ms1 => self.ms1_isotopes.as_deref(),
                MassLevel::Ms2 => self.ms2_isotopes.as_deref().map(Vec::as_slice),
            }
        } else {
            None
        };
        let current_view = SpectrumView {
            peaks,
            limits,
            floor,
            isotopes: current_isotopes,
            focused_mz: self
                .focused_peak
                .filter(|_| !self.focused_reference)
                .map(|peak| peak.mz),
            style: self.bar_style,
            tone: SpectrumTone::Current,
            inverted: comparison.is_some(),
        };
        if let Some((pinned, reference_peaks, upper, lower, center)) = comparison {
            let reference_limits = mass_limits(reference_peaks, window);
            let reference_view = SpectrumView {
                peaks: reference_peaks,
                limits: reference_limits,
                floor: reference_limits.2 * cutoff,
                isotopes: if self.isotope_highlight {
                    pinned.isotopes.as_deref().map(Vec::as_slice)
                } else {
                    None
                },
                focused_mz: self
                    .focused_peak
                    .filter(|_| self.focused_reference)
                    .map(|peak| peak.mz),
                style: self.bar_style,
                tone: SpectrumTone::Reference,
                inverted: false,
            };
            spectrum_plot::draw(&reference_view, upper, frame.buffer_mut());
            for x in self.spectrum_bars.x..self.spectrum_bars.right() {
                paint(frame.buffer_mut(), x, center, "─", Color::DarkGray);
            }
            spectrum_plot::draw(&current_view, lower, frame.buffer_mut());
        } else {
            spectrum_plot::draw(&current_view, self.spectrum_bars, frame.buffer_mut());
        }
        if spec_rect.height >= 5 {
            spectrum_labels(
                peaks,
                Rect::new(spec_rect.x, spec_rect.bottom() - 2, spec_rect.width, 1),
                min_mz,
                max_mz,
                floor,
                frame.buffer_mut(),
            );
        }
        horizontal_axis(
            frame.buffer_mut(),
            Rect::new(spec_rect.x, spec_rect.bottom() - 1, spec_rect.width, 1),
            min_mz,
            max_mz,
            "m/z",
        );
    }

    fn render(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        if area.width < 30 || area.height < 12 {
            self.chart = Rect::default();
            self.spectrum_bars = Rect::default();
            frame.render_widget(
                Paragraph::new(
                    "Terminal too small (need at least 30×12). Resize or press q to quit.",
                ),
                area,
            );
            return;
        }
        self.ensure_isotopes();
        let files_visible = self.file_panel.as_ref().is_some_and(|panel| panel.visible);
        let features_visible = self
            .feature_panel
            .as_ref()
            .is_some_and(|panel| panel.visible);
        let side_width = if files_visible || features_visible {
            if area.width < 80 || area.height < 20 {
                if let Some(panel) = &mut self.file_panel {
                    panel.visible = false;
                }
                if let Some(panel) = &mut self.feature_panel {
                    panel.visible = false;
                }
                self.status = "Sidebar hidden: resize to at least 80×20 and press g or f".into();
                0
            } else {
                if features_visible && !files_visible {
                    50
                } else {
                    (area.width / 3).clamp(30, 50)
                }
            }
        } else {
            0
        };
        let view = Rect::new(area.x, area.y, area.width - side_width, area.height);
        if side_width > 0 {
            let side = Rect::new(view.right() + 1, area.y, side_width - 1, area.height);
            if files_visible {
                let catalog = &self.catalog;
                let files = &self.files;
                let generation = self.chrom_generation;
                let active = self.active_file;
                self.file_panel.as_mut().expect("file panel open").render(
                    frame,
                    side,
                    catalog.len(),
                    active,
                    |index| FileRow {
                        label: &catalog[index].relative,
                        enabled: files[index].enabled,
                        state: if files[index].error.is_some() {
                            FileLoadState::Failed
                        } else if files[index].queued.is_some() {
                            FileLoadState::Loading
                        } else if files[index].generation == generation
                            && (index == active || files[index].cached.is_some())
                        {
                            FileLoadState::Ready
                        } else {
                            FileLoadState::Unopened
                        },
                        has_audit: catalog[index].audit.is_some(),
                    },
                );
            } else if features_visible {
                self.feature_panel
                    .as_mut()
                    .expect("feature panel open")
                    .render(frame, side, &self.data, self.rt_minutes);
            }
            for y in area.y..area.bottom() {
                paint(frame.buffer_mut(), view.right(), y, "│", Color::DarkGray);
            }
        }
        let plot_height = if self.mirror_reference().is_some() {
            ((view.height - 5) / 2).max(5)
        } else {
            ((view.height - 5) * 3 / 5).max(5)
        };
        let header = Rect::new(view.x, view.y, view.width, 2);
        self.chart = Rect::new(view.x, view.y + 2, view.width, plot_height);
        let spectrum = Rect::new(
            view.x,
            self.chart.y + plot_height,
            view.width,
            view.height - 4 - plot_height,
        );
        let footer = Rect::new(view.x, view.y + view.height - 2, view.width, 2);

        let visible = self
            .data
            .features
            .iter()
            .filter(|feature| {
                self.xic
                    .is_none_or(|(mz, tolerance)| (feature.mz - mz).abs() <= tolerance)
            })
            .count();
        let enabled = self.files.iter().filter(|file| file.enabled).count();
        let ready = (0..self.catalog.len())
            .filter(|&index| self.file_data(index).is_some())
            .count();
        let title = if self.catalog.len() > 1 {
            format!(
                "{} | {}/{} {} | {ready}/{enabled} traces | {visible} accepted",
                self.name,
                self.active_file + 1,
                self.catalog.len(),
                self.catalog[self.active_file].relative.display()
            )
        } else {
            format!("{} | {} accepted features", self.name, visible)
        };
        frame.render_widget(Paragraph::new(title), header);
        let max = self.chart_scale();
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(format!(
                    "MS1 intensity ≤ {:.3e}  • chromatogram   ◆ accepted  ★ selected",
                    max
                )),
            self.chart,
        );
        let rect = plot_rect(self.chart);
        for index in 0..self.catalog.len() {
            if index != self.active_file {
                if let Some(data) = self.file_data(index) {
                    chrom_line(data, self.window, rect, max, file_color(index), frame);
                }
            }
        }
        if let Some(data) = self.file_data(self.active_file) {
            let color = if self.catalog.len() == 1 {
                Color::Cyan
            } else {
                file_color(self.active_file)
            };
            chrom_line(data, self.window, rect, max, color, frame);
        }
        {
            let buffer = frame.buffer_mut();
            if self.files[self.active_file].enabled {
                for index in 0..self.data.features.len() {
                    if self.selected == Some(index) {
                        continue;
                    }
                    if let Some((x, y)) =
                        marker(&self.data, index, self.window, rect, max, self.xic)
                    {
                        paint(buffer, x, y, "◆", Color::Magenta);
                    }
                }
                if let Some(index) = self.selected {
                    if let Some((x, y)) =
                        marker(&self.data, index, self.window, rect, max, self.xic)
                    {
                        paint(buffer, x, y, "★", Color::Yellow);
                    }
                }
            }
            horizontal_axis(
                buffer,
                Rect::new(rect.x, rect.bottom(), rect.width, 1),
                self.display_rt(self.window.start),
                self.display_rt(self.window.end),
                if self.rt_minutes {
                    "RT (min)"
                } else {
                    "RT (s)"
                },
            );
        }
        let label = match (self.level, self.selected, self.selected_rt) {
            (MassLevel::Ms1, Some(index), _) => {
                let feature = &self.data.features[index];
                format!(
                    "MS1 feature {} | RT {:.3} {}",
                    feature.id,
                    self.display_rt(feature.rt_seconds),
                    self.rt_unit()
                )
            }
            (MassLevel::Ms1, None, Some(rt)) => {
                format!(
                    "MS1 scan near RT {:.3} {}",
                    self.display_rt(rt),
                    self.rt_unit()
                )
            }
            (MassLevel::Ms1, None, None) => "MS1 — click the chromatogram".to_owned(),
            (MassLevel::Ms2, Some(index), _) => {
                let feature = &self.data.features[index];
                let precursor = self
                    .spectrum
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .and_then(|spectra| spectra.precursor.as_ref());
                let metadata = match precursor {
                    Some(p) => p.charge.map_or_else(
                        || format!("precursor m/z {:.5}", p.mz),
                        |charge| format!("precursor m/z {:.5} z={charge}", p.mz),
                    ),
                    None if self.pending_spectrum => "precursor loading".to_owned(),
                    None => "precursor unavailable".to_owned(),
                };
                if let Some(pinned) = self.mirror_reference() {
                    format!(
                        "MS2 {} | ↑{} ↓{} | scan {} | RT {:.3} {}",
                        metadata,
                        pinned.feature_id,
                        feature.id,
                        feature.ms2_scan_id,
                        self.display_rt(feature.rt_seconds),
                        self.rt_unit()
                    )
                } else {
                    format!(
                        "MS2 {} | feature {} scan {} | RT {:.3} {} | apex {:.3e}",
                        metadata,
                        feature.id,
                        feature.ms2_scan_id,
                        self.display_rt(feature.rt_seconds),
                        self.rt_unit(),
                        feature.apex_intensity
                    )
                }
            }
            (MassLevel::Ms2, None, _) => "MS2 — click an accepted feature marker".to_owned(),
        };
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(label),
            spectrum,
        );
        let spec_rect = Rect::new(
            spectrum.x + 1,
            spectrum.y + 1,
            spectrum.width.saturating_sub(2),
            spectrum.height.saturating_sub(2),
        );
        self.spectrum_bars = if spec_rect.height >= 5 {
            Rect::new(
                spec_rect.x,
                spec_rect.y + 1,
                spec_rect.width,
                spec_rect.height - 3,
            )
        } else if spec_rect.height >= 3 {
            Rect::new(
                spec_rect.x,
                spec_rect.y + 1,
                spec_rect.width,
                spec_rect.height - 2,
            )
        } else {
            Rect::default()
        };
        if let Some(peaks) = self.visible_peaks() {
            if let Some(full) = self.full_spectrum_window() {
                self.render_spectrum(frame, spec_rect, peaks, full);
            }
        } else if let Some(Err(error)) = &self.spectrum {
            frame.render_widget(
                Paragraph::new(format!("Cannot display spectrum: {error}")),
                spec_rect,
            );
        } else if self.pending_spectrum {
            frame.render_widget(
                Paragraph::new("Loading selected scan(s)… you can keep navigating."),
                spec_rect,
            );
        } else if self.level == MassLevel::Ms2 {
            frame.render_widget(
                Paragraph::new("Select a ◆ to display its MS2, or press 1 for MS1."),
                spec_rect,
            );
        } else {
            frame.render_widget(
                Paragraph::new("Click the chromatogram to display the nearest MS1 scan."),
                spec_rect,
            );
        }
        let footer_text = if let Some(input) = &self.mass_search {
            format!(
                "Exact m/z > {input}█  Enter XIC  Esc cancel\n{}",
                self.status
            )
        } else {
            let controls = if self.catalog.len() > 1 {
                "t full  g files  n/b next/prev  o trace  f features  +/- RT  / m/z  1/2 MS  p pin"
            } else if side_width > 0 {
                "t full  f list  p pin  i iso  v style  h/l peaks  wheel zoom  +/- RT  m MS  / XIC"
            } else {
                "t full  q quit  f features  +/- RT  wheel zoom  h/l peaks  v style  i isotopes  p pin  1/2 MS  / m/z"
            };
            format!("{controls}\n{}", self.status)
        };
        frame.render_widget(Paragraph::new(footer_text), footer);
    }
}

/// Inspect a raw mzXML/mzML file or navigate and overlay files in a directory.
pub fn run(
    raw: &Path,
    audit: Option<&Path>,
    feature_list: Option<&Path>,
    mode: Chromatogram,
) -> Result<()> {
    let name = match &mode {
        Chromatogram::Tic => "TIC".to_owned(),
        Chromatogram::Bpi => "BPI".to_owned(),
        Chromatogram::Xic { mz, .. } => format!("XIC m/z {mz:.5}"),
    };
    let xic = inspect::xic_window(mode)?;
    let full_mode = if matches!(mode, Chromatogram::Bpi) {
        Chromatogram::Bpi
    } else {
        Chromatogram::Tic
    };
    let (ppm, absolute_mz) = match mode {
        Chromatogram::Xic {
            ppm, absolute_mz, ..
        } => (ppm, absolute_mz),
        _ => (10.0, 0.003),
    };
    let catalog: Arc<[MsFile]> = explore_files::discover_files(raw, audit, feature_list)?.into();
    let LoadedFile {
        data,
        order,
        feature_panel,
    } = explore_loading::load(&catalog[0], mode)?;
    let full = full_window(&data);
    let files = (0..catalog.len())
        .map(|index| FileState {
            cached: None,
            enabled: index == 0,
            generation: 0,
            queued: None,
            error: None,
        })
        .collect();
    let file_panel = if catalog.len() > 1 {
        let mut panel = FilePanel::new();
        panel.visible = true;
        Some(panel)
    } else {
        None
    };
    let background = Background::start(&catalog[0].raw);
    let loader = FileLoader::start(Arc::clone(&catalog));
    let status = catalog[0].warning.clone().unwrap_or_else(|| {
        "Click a chromatogram point for MS1; g files, n/b navigate, Space overlays".into()
    });
    let mut viewer = Viewer {
        data,
        catalog,
        files,
        active_file: 0,
        pending_file: None,
        file_panel,
        loader,
        mode,
        full_mode,
        chrom_generation: 0,
        order,
        name,
        full,
        window: full,
        rt_minutes: true,
        selected: None,
        selected_rt: None,
        level: MassLevel::Ms1,
        spectrum_cutoff: DEFAULT_SPECTRUM_CUTOFF,
        bar_style: BarStyle::Blocks,
        isotope_highlight: false,
        ms1_isotopes: None,
        ms2_isotopes: None,
        pinned_ms2: None,
        spectrum: None,
        focused_peak: None,
        focused_reference: false,
        spectrum_window: None,
        background,
        spectrum_generation: 0,
        trace_generation: 0,
        pending_spectrum: false,
        pending_trace: None,
        xic,
        ppm,
        absolute_mz,
        status,
        mass_search: None,
        feature_panel,
        chart: Rect::default(),
        spectrum_bars: Rect::default(),
    };
    let _session = TerminalSession::start()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("initializing terminal viewer")?;
    event_loop(&mut terminal, &mut viewer)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    viewer: &mut Viewer,
) -> Result<()> {
    let mut dirty = true;
    loop {
        dirty |= viewer.poll_results();
        dirty |= viewer.poll_files();
        if dirty {
            terminal
                .draw(|frame| viewer.render(frame))
                .context("drawing interactive viewer")?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(30)).context("waiting for terminal input")? {
            continue;
        }
        dirty = true;
        match event::read().context("reading terminal event")? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if viewer.mass_search.is_some() {
                    viewer.search_key(key.code);
                    continue;
                }
                if viewer
                    .file_panel
                    .as_ref()
                    .is_some_and(|panel| panel.visible)
                {
                    let action = viewer
                        .file_panel
                        .as_mut()
                        .expect("file panel open")
                        .key(key.code, viewer.catalog.len());
                    if !matches!(action, FileAction::Pass) {
                        viewer.file_action(action);
                        continue;
                    }
                }
                if viewer
                    .feature_panel
                    .as_ref()
                    .is_some_and(|panel| panel.visible)
                {
                    let action = viewer
                        .feature_panel
                        .as_mut()
                        .expect("panel open")
                        .key(key.code);
                    if !matches!(action, PanelAction::Pass) {
                        viewer.panel_action(action);
                        continue;
                    }
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('/') => {
                        viewer.mass_search = Some(String::new());
                        viewer.status = "Type an exact positive m/z; Backspace edits".into();
                    }
                    KeyCode::Char('f') => {
                        if let Some(panel) = &mut viewer.feature_panel {
                            if let Some(files) = &mut viewer.file_panel {
                                files.visible = false;
                            }
                            panel.visible = true;
                        } else {
                            viewer.status = "Feature list unavailable for this file: provide its *_feature_list.tsv and audit".into();
                        }
                    }
                    KeyCode::Char('g') => {
                        if let Some(panel) = &mut viewer.file_panel {
                            if let Some(features) = &mut viewer.feature_panel {
                                features.visible = false;
                            }
                            panel.visible = !panel.visible;
                        } else {
                            viewer.status =
                                "Only one MS file is open; pass a directory to browse files".into();
                        }
                    }
                    KeyCode::Char('n') => viewer.cycle_file(true),
                    KeyCode::Char('b') => viewer.cycle_file(false),
                    KeyCode::Char('o') | KeyCode::Char(' ') => {
                        viewer.toggle_file(viewer.active_file)
                    }
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
                    KeyCode::Char('h') => {
                        viewer.spectrum_cutoff = viewer.spectrum_cutoff.saturating_sub(1);
                        viewer.focused_peak = None;
                    }
                    KeyCode::Char('l') => {
                        viewer.spectrum_cutoff =
                            (viewer.spectrum_cutoff + 1).min(SPECTRUM_CUTOFFS.len() - 1);
                        viewer.focused_peak = None;
                    }
                    KeyCode::Char('v') => {
                        viewer.bar_style = if viewer.bar_style == BarStyle::Blocks {
                            BarStyle::Thin
                        } else {
                            BarStyle::Blocks
                        };
                        viewer.status = match viewer.bar_style {
                            BarStyle::Thin => "Thin spectrum sticks (v for eighth-block bars)",
                            BarStyle::Blocks => {
                                "Fractional-height spectrum bars (v for thin sticks)"
                            }
                        }
                        .into();
                    }
                    KeyCode::Char('i') => viewer.toggle_isotopes(),
                    KeyCode::Char('p') => viewer.toggle_pin(),
                    KeyCode::Char('1') => viewer.show_level(MassLevel::Ms1),
                    KeyCode::Char('2') => viewer.show_level(MassLevel::Ms2),
                    KeyCode::Char('m') => viewer.show_level(if viewer.level == MassLevel::Ms1 {
                        MassLevel::Ms2
                    } else {
                        MassLevel::Ms1
                    }),
                    KeyCode::Char('u') => viewer.rt_minutes = !viewer.rt_minutes,
                    KeyCode::Char('t') => viewer.restore_full(),
                    KeyCode::Char('r') => {
                        viewer.window = viewer.full;
                        viewer.spectrum_window = None;
                    }
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        viewer.cycle(true)
                    }
                    KeyCode::Tab | KeyCode::Right => viewer.cycle(false),
                    KeyCode::BackTab | KeyCode::Left => viewer.cycle(true),
                    _ => {}
                }
            }
            Event::Mouse(mouse)
                if viewer
                    .file_panel
                    .as_ref()
                    .is_some_and(|panel| panel.visible)
                    && mouse.column >= viewer.chart.right() =>
            {
                let count = viewer.catalog.len();
                let panel = viewer.file_panel.as_mut().expect("file panel open");
                let action = match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        panel.click(mouse.column, mouse.row, count)
                    }
                    MouseEventKind::ScrollUp => panel.key(KeyCode::Up, count),
                    MouseEventKind::ScrollDown => panel.key(KeyCode::Down, count),
                    _ => FileAction::Stay,
                };
                viewer.file_action(action);
            }
            Event::Mouse(mouse)
                if viewer
                    .feature_panel
                    .as_ref()
                    .is_some_and(|panel| panel.visible)
                    && mouse.column >= viewer.chart.right() =>
            {
                let panel = viewer.feature_panel.as_mut().expect("panel open");
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => panel.click(mouse.column, mouse.row),
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => panel.scroll(
                        mouse.column,
                        mouse.row,
                        mouse.kind == MouseEventKind::ScrollUp,
                    ),
                    _ => {}
                }
            }
            Event::Mouse(_) if viewer.mass_search.is_some() => {}
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if mouse.row >= viewer.spectrum_bars.y && viewer.spectrum_bars.height > 0 {
                        viewer.click_spectrum(mouse.column, mouse.row);
                    } else {
                        viewer.click(mouse.column, mouse.row);
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let factor = if mouse.kind == MouseEventKind::ScrollUp {
                        1.0 / 1.5
                    } else {
                        1.5
                    };
                    if viewer.spectrum_bars.height > 0
                        && mouse.row >= viewer.chart.bottom()
                        && mouse.row <= viewer.spectrum_bars.bottom()
                    {
                        viewer.zoom_mass(mouse.column, factor);
                    } else {
                        let rect = plot_rect(viewer.chart);
                        let anchor = if rect.width > 0
                            && mouse.column >= rect.x
                            && mouse.column < rect.right()
                        {
                            viewer.window.at(mouse.column, rect)
                        } else {
                            (viewer.window.start + viewer.window.end) / 2.0
                        };
                        viewer.window.zoom(viewer.full, anchor, factor);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod spectrum_display_tests {
    use super::*;

    #[test]
    fn spectrum_labels_name_visible_strong_peaks_including_mass_range_ends() {
        let peaks = [
            Peak {
                mz: 100.0,
                intensity: 100.0,
            },
            Peak {
                mz: 200.0,
                intensity: 0.5,
            },
            Peak {
                mz: 300.0,
                intensity: 60.0,
            },
        ];
        let rect = Rect::new(0, 0, 21, 1);
        let mut buffer = Buffer::empty(rect);
        spectrum_labels(&peaks, rect, 100.0, 300.0, 5.0, &mut buffer);
        assert_eq!(
            buffer[(0, 0)].symbol(),
            "1",
            "base-peak m/z must be labeled"
        );
        assert_eq!(
            buffer[(15, 0)].symbol(),
            "3",
            "strong fragment m/z must be labeled"
        );
        assert_eq!(
            buffer[(7, 0)].symbol(),
            " ",
            "hidden noise must not receive a label"
        );
    }
}

#[cfg(test)]
mod chromatogram_overlay_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn has_color(terminal: &Terminal<TestBackend>, color: Color) -> bool {
        let buffer = terminal.backend().buffer();
        (0..10).any(|y| {
            (0..50).any(|x| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.symbol() != " " && cell.fg == color)
            })
        })
    }

    #[test]
    fn independent_raw_traces_keep_their_colors_and_can_be_hidden() {
        let a = Dataset {
            points: [(0.0, 1.0), (1.0, 2.0), (2.0, 1.0)]
                .into_iter()
                .map(|(rt_seconds, intensity)| explore_data::ChromPoint {
                    rt_seconds,
                    intensity,
                })
                .collect(),
            features: Vec::new(),
        };
        let b = Dataset {
            points: [(0.0, 8.0), (1.0, 9.0), (2.0, 8.0)]
                .into_iter()
                .map(|(rt_seconds, intensity)| explore_data::ChromPoint {
                    rt_seconds,
                    intensity,
                })
                .collect(),
            features: Vec::new(),
        };
        let window = Window {
            start: 0.0,
            end: 2.0,
        };
        let rect = Rect::new(0, 0, 50, 10);
        let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();
        terminal
            .draw(|frame| {
                chrom_line(&a, window, rect, 10.0, Color::Red, frame);
                chrom_line(&b, window, rect, 10.0, Color::Blue, frame);
            })
            .unwrap();
        assert!(
            has_color(&terminal, Color::Red),
            "first file trace stays visible under the second"
        );
        assert!(
            has_color(&terminal, Color::Blue),
            "second file trace appears in its own color"
        );
        terminal
            .draw(|frame| chrom_line(&a, window, rect, 10.0, Color::Red, frame))
            .unwrap();
        assert!(has_color(&terminal, Color::Red));
        assert!(
            !has_color(&terminal, Color::Blue),
            "turning a trace off removes only its own color"
        );
    }
}
