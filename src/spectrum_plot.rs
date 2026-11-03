//! Spectrum sticks for normal and mirrored terminal plots.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

use crate::{spectrum::Peak, spectrum_isotopes::IsotopeMatch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarStyle {
    Thin,
    Blocks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpectrumTone {
    Current,
    Reference,
}

pub(crate) struct SpectrumView<'a> {
    pub peaks: &'a [Peak],
    /// Visible minimum m/z, maximum m/z, and base intensity for this view.
    pub limits: (f64, f64, f64),
    pub floor: f64,
    pub isotopes: Option<&'a [Option<IsotopeMatch>]>,
    pub focused_mz: Option<f64>,
    pub style: BarStyle,
    pub tone: SpectrumTone,
    pub inverted: bool,
}

const LOWER_BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
const UPPER_BLOCKS: [&str; 9] = [" ", "▔", "🮂", "🮃", "▀", "🮄", "🮅", "🮆", "█"];
// Resolve collisions in fixed-size column batches rather than allocating per frame.
const COLUMN_BATCH: usize = 128;

pub(crate) fn visible_peak(peak: &Peak, min_mz: f64, max_mz: f64, floor: f64) -> bool {
    peak.mz.is_finite()
        && peak.mz >= min_mz
        && peak.mz <= max_mz
        && peak.intensity.is_finite()
        && peak.intensity > 0.0
        && peak.intensity >= floor
}

pub(crate) fn peak_column(mz: f64, rect: Rect, min_mz: f64, max_mz: f64) -> u16 {
    let fraction = if max_mz > min_mz {
        ((mz - min_mz) / (max_mz - min_mz)).clamp(0.0, 1.0)
    } else {
        0.5
    };
    rect.x + (fraction * f64::from(rect.width.saturating_sub(1))).round() as u16
}

fn height_units(peak: &Peak, view: &SpectrumView<'_>, height: u16) -> usize {
    let per_cell = match view.style {
        BarStyle::Thin => 1,
        BarStyle::Blocks => 8,
    };
    let capacity = usize::from(height) * per_cell;
    let scale = if view.limits.2 > 0.0 {
        view.limits.2
    } else {
        1.0
    };
    ((peak.intensity / scale * capacity as f64).round() as usize).clamp(1, capacity)
}

fn bar_color(view: &SpectrumView<'_>, index: usize, peak: &Peak) -> Color {
    if view.focused_mz == Some(peak.mz) {
        Color::Yellow
    } else if view
        .isotopes
        .is_some_and(|matches| matches.get(index).is_some_and(Option::is_some))
    {
        Color::Magenta
    } else if view.tone == SpectrumTone::Reference {
        Color::Cyan
    } else if peak.intensity >= view.limits.2 * 0.25 {
        Color::LightGreen
    } else if peak.intensity >= view.limits.2 * 0.10 {
        Color::Green
    } else {
        Color::DarkGray
    }
}

fn paint_bar(view: &SpectrumView<'_>, rect: Rect, buffer: &mut Buffer, x: u16, index: usize) {
    let peak = &view.peaks[index];
    let units = height_units(peak, view, rect.height);
    let color = bar_color(view, index, peak);
    let baseline = if view.inverted {
        rect.y
    } else {
        rect.bottom() - 1
    };
    let row = |offset: usize| {
        if view.inverted {
            baseline + offset as u16
        } else {
            baseline - offset as u16
        }
    };
    let mut paint = |offset, symbol| {
        if let Some(cell) = buffer.cell_mut((x, row(offset))) {
            cell.set_symbol(symbol)
                .set_style(Style::default().fg(color));
        }
    };
    match view.style {
        BarStyle::Thin => {
            for offset in 0..units {
                paint(offset, "│");
            }
        }
        BarStyle::Blocks => {
            for offset in 0..units / 8 {
                paint(offset, "█");
            }
            let fraction = units % 8;
            if fraction > 0 {
                let blocks = if view.inverted {
                    &UPPER_BLOCKS
                } else {
                    &LOWER_BLOCKS
                };
                paint(units / 8, blocks[fraction]);
            }
        }
    }
}

/// Draw each column's strongest visible centroid, regardless of input order.
pub(crate) fn draw(view: &SpectrumView<'_>, rect: Rect, buffer: &mut Buffer) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let (min_mz, max_mz, _) = view.limits;
    for start in (0..usize::from(rect.width)).step_by(COLUMN_BATCH) {
        let len = (usize::from(rect.width) - start).min(COLUMN_BATCH);
        let mut winners: [Option<usize>; COLUMN_BATCH] = [None; COLUMN_BATCH];
        for (index, peak) in view.peaks.iter().enumerate() {
            if !visible_peak(peak, min_mz, max_mz, view.floor) {
                continue;
            }
            let column = usize::from(peak_column(peak.mz, rect, min_mz, max_mz) - rect.x);
            if column < start || column >= start + len {
                continue;
            }
            let winner = &mut winners[column - start];
            if winner.is_none_or(|previous| peak.intensity > view.peaks[previous].intensity) {
                *winner = Some(index);
            }
        }
        for (offset, winner) in winners[..len].iter().enumerate() {
            if let Some(index) = winner {
                paint_bar(view, rect, buffer, rect.x + (start + offset) as u16, *index);
            }
        }
    }
}

/// Hit only the displayed extent of the winning stick in this exact column.
pub(crate) fn peak_at(view: &SpectrumView<'_>, rect: Rect, x: u16, y: u16) -> Option<Peak> {
    if rect.width == 0
        || rect.height == 0
        || x < rect.x
        || x >= rect.right()
        || y < rect.y
        || y >= rect.bottom()
    {
        return None;
    }
    let (min_mz, max_mz, _) = view.limits;
    let mut winner: Option<Peak> = None;
    for peak in view.peaks.iter() {
        if visible_peak(peak, min_mz, max_mz, view.floor)
            && peak_column(peak.mz, rect, min_mz, max_mz) == x
            && winner.is_none_or(|previous| peak.intensity > previous.intensity)
        {
            winner = Some(*peak);
        }
    }
    winner.filter(|peak| {
        let units = height_units(peak, view, rect.height);
        let offset = if view.inverted {
            y - rect.y
        } else {
            rect.bottom() - 1 - y
        };
        match view.style {
            BarStyle::Thin => usize::from(offset) < units,
            BarStyle::Blocks => usize::from(offset) <= (units - 1) / 8,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(peaks: &'a [Peak], style: BarStyle, inverted: bool) -> SpectrumView<'a> {
        SpectrumView {
            peaks,
            limits: (100.0, 200.0, 100.0),
            floor: 0.0,
            isotopes: None,
            focused_mz: None,
            style,
            tone: SpectrumTone::Current,
            inverted,
        }
    }

    #[test]
    fn thin_and_fractional_sticks_have_distinct_shapes() {
        let peaks = [
            Peak {
                mz: 100.0,
                intensity: 100.0,
            },
            Peak {
                mz: 150.0,
                intensity: 5.0,
            },
        ];
        let rect = Rect::new(0, 0, 21, 8);
        let mut buffer = Buffer::empty(rect);
        draw(&view(&peaks, BarStyle::Blocks, false), rect, &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "█");
        assert_eq!(buffer[(10, 7)].symbol(), "▃");
        let mut buffer = Buffer::empty(rect);
        draw(&view(&peaks, BarStyle::Thin, false), rect, &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "│");
        assert_eq!(buffer[(10, 7)].symbol(), "│");
        assert_eq!(buffer[(10, 6)].symbol(), " ");
    }

    #[test]
    fn inverted_bars_extend_down_from_top_and_click_only_painted_rows() {
        let peaks = [
            Peak {
                mz: 100.0,
                intensity: 100.0,
            },
            Peak {
                mz: 150.0,
                intensity: 10.0,
            },
        ];
        let rect = Rect::new(0, 0, 21, 8);
        for style in [BarStyle::Thin, BarStyle::Blocks] {
            let normal = view(&peaks, style, false);
            let inverted = view(&peaks, style, true);
            let mut buffer = Buffer::empty(rect);
            draw(&normal, rect, &mut buffer);
            assert_ne!(buffer[(10, 7)].symbol(), " ");
            assert_eq!(buffer[(10, 0)].symbol(), " ");
            let mut mirror = Buffer::empty(rect);
            draw(&inverted, rect, &mut mirror);
            assert_ne!(mirror[(10, 0)].symbol(), " ");
            assert_eq!(mirror[(10, 7)].symbol(), " ");
            assert_eq!(
                peak_at(&inverted, rect, 10, 0).map(|peak| peak.mz),
                Some(150.0)
            );
            assert!(peak_at(&inverted, rect, 10, 7).is_none());
        }
    }

    #[test]
    fn blocks_reverse_fractional_tips_for_mirrored_reference() {
        let peaks = [Peak {
            mz: 150.0,
            intensity: 90.0,
        }];
        let rect = Rect::new(0, 0, 21, 8);
        let mut normal = Buffer::empty(rect);
        let mut mirror = Buffer::empty(rect);
        let mut display = view(&peaks, BarStyle::Blocks, false);
        draw(&display, rect, &mut normal);
        assert_eq!(normal[(10, 0)].symbol(), "▂");
        display.inverted = true;
        display.tone = SpectrumTone::Reference;
        draw(&display, rect, &mut mirror);
        assert_eq!(mirror[(10, 7)].symbol(), "🮂");
        assert_eq!(mirror[(10, 0)].fg, Color::Cyan);
        assert_eq!(
            peak_at(&display, rect, 10, 7).map(|peak| peak.mz),
            Some(150.0)
        );
    }

    #[test]
    fn strongest_column_peak_wins_in_both_orders_even_within_one_row() {
        let stronger = Peak {
            mz: 150.0,
            intensity: 10.0,
        };
        let weaker = Peak {
            mz: 150.1,
            intensity: 9.5,
        };
        let base = Peak {
            mz: 200.0,
            intensity: 100.0,
        };
        let rect = Rect::new(0, 0, 21, 8);
        for peaks in [[base, stronger, weaker], [base, weaker, stronger]] {
            let mut buffer = Buffer::empty(rect);
            let display = view(&peaks, BarStyle::Blocks, false);
            draw(&display, rect, &mut buffer);
            assert_eq!(buffer[(10, 7)].symbol(), "▆");
            assert_eq!(buffer[(10, 7)].fg, Color::Green);
            assert_eq!(
                peak_at(&display, rect, 10, 7).map(|peak| peak.mz),
                Some(150.0)
            );
        }
    }

    #[test]
    fn isotopes_and_focus_coloring_respect_cutoff_and_visible_base() {
        let peaks = [
            Peak {
                mz: 100.0,
                intensity: 100.0,
            },
            Peak {
                mz: 150.0,
                intensity: 25.0,
            },
            Peak {
                mz: 200.0,
                intensity: 4.0,
            },
        ];
        let isotope = [
            None,
            Some(IsotopeMatch {
                parent: 0,
                order: 1,
            }),
            None,
        ];
        let rect = Rect::new(0, 0, 21, 8);
        let mut display = view(&peaks, BarStyle::Blocks, false);
        display.isotopes = Some(&isotope);
        display.floor = 5.0;
        let mut buffer = Buffer::empty(rect);
        draw(&display, rect, &mut buffer);
        assert_eq!(buffer[(10, 7)].fg, Color::Magenta);
        assert_eq!(buffer[(20, 7)].symbol(), " ");
        assert!(peak_at(&display, rect, 20, 7).is_none());
        display.focused_mz = Some(150.0);
        let mut focused = Buffer::empty(rect);
        draw(&display, rect, &mut focused);
        assert_eq!(focused[(10, 7)].fg, Color::Yellow);
    }

    #[test]
    fn each_view_uses_its_own_visible_base_for_height() {
        let peaks = [Peak {
            mz: 150.0,
            intensity: 20.0,
        }];
        let rect = Rect::new(0, 0, 21, 8);
        let mut display = view(&peaks, BarStyle::Thin, false);
        display.limits.2 = 20.0;
        let mut current = Buffer::empty(rect);
        draw(&display, rect, &mut current);
        assert_eq!(current[(10, 0)].symbol(), "│");
        display.limits.2 = 100.0;
        display.tone = SpectrumTone::Reference;
        let mut reference = Buffer::empty(rect);
        draw(&display, rect, &mut reference);
        assert_eq!(reference[(10, 0)].symbol(), " ");
        assert_eq!(reference[(10, 6)].symbol(), "│");
    }

    #[test]
    fn fractional_tip_click_selects_drawn_mass_but_not_unpainted_neighbor() {
        let peaks = [
            Peak {
                mz: 100.0,
                intensity: 100.0,
            },
            Peak {
                mz: 200.0,
                intensity: 90.0,
            },
        ];
        let rect = Rect::new(0, 0, 21, 8);
        let display = view(&peaks, BarStyle::Blocks, false);
        let mut buffer = Buffer::empty(rect);
        draw(&display, rect, &mut buffer);
        assert_eq!(buffer[(20, 0)].symbol(), "▂");
        assert_eq!(
            peak_at(&display, rect, 20, 0).map(|peak| peak.mz),
            Some(200.0)
        );
        assert!(peak_at(&display, rect, 19, 0).is_none());
    }
}
