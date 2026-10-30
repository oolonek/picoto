use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::spectrum::{Peak, read_spectra};

#[derive(Clone, Copy)]
pub enum Chromatogram {
    Tic,
    Bpi,
    Xic { mz: f64, ppm: f64, absolute_mz: f64 },
}

fn valid_peak(peak: &Peak) -> Result<()> {
    ensure!(
        peak.mz.is_finite()
            && peak.mz >= 0.0
            && peak.intensity.is_finite()
            && peak.intensity >= 0.0,
        "scan contains a non-finite or negative m/z or intensity"
    );
    Ok(())
}

pub(crate) fn xic_window(mode: Chromatogram) -> Result<Option<(f64, f64)>> {
    match mode {
        Chromatogram::Xic {
            mz,
            ppm,
            absolute_mz,
        } => {
            ensure!(
                mz.is_finite()
                    && mz > 0.0
                    && ppm.is_finite()
                    && ppm >= 0.0
                    && absolute_mz.is_finite()
                    && absolute_mz >= 0.0,
                "XIC m/z and tolerances must be finite and nonnegative (m/z positive)"
            );
            let tolerance = (ppm * mz / 1_000_000.0).max(absolute_mz);
            ensure!(tolerance.is_finite(), "XIC tolerance is too large");
            Ok(Some((mz, tolerance)))
        }
        _ => Ok(None),
    }
}

pub(crate) fn chromatogram_points(
    input: &Path,
    mode: Chromatogram,
    rt_range: Option<(f64, f64)>,
    point: &mut dyn FnMut(f64, f64) -> Result<()>,
) -> Result<usize> {
    if let Some((start, end)) = rt_range {
        ensure!(
            start.is_finite() && end.is_finite() && start >= 0.0 && start <= end,
            "invalid retention time interval"
        );
    }
    let xic = xic_window(mode)?;
    let mut rows = 0;
    read_spectra(input, &mut |scan| {
        if scan.ms_level != 1 || !scan.centroided {
            return Ok(());
        }
        ensure!(
            scan.rt_seconds.is_finite() && scan.rt_seconds >= 0.0,
            "scan {}: invalid retention time",
            scan.id
        );
        if rt_range.is_some_and(|(start, end)| scan.rt_seconds < start || scan.rt_seconds > end) {
            return Ok(());
        }
        let mut intensity: f64 = 0.0;
        for peak in &scan.peaks {
            valid_peak(peak).with_context(|| format!("scan {}", scan.id))?;
            match mode {
                Chromatogram::Tic => intensity += peak.intensity,
                Chromatogram::Bpi => intensity = intensity.max(peak.intensity),
                Chromatogram::Xic { .. } => {
                    let (mz, tolerance) = xic.expect("validated XIC parameters");
                    if (peak.mz - mz).abs() <= tolerance {
                        intensity += peak.intensity;
                    }
                }
            }
        }
        ensure!(
            intensity.is_finite(),
            "scan {}: intensity sum overflows",
            scan.id
        );
        point(scan.rt_seconds, intensity)?;
        rows += 1;
        Ok(())
    })?;
    if rows == 0 {
        bail!("no centroided MS1 scans in the requested retention time interval");
    }
    Ok(rows)
}

pub fn chromatogram(
    input: &Path,
    mode: Chromatogram,
    rt_range: Option<(f64, f64)>,
    out: &mut dyn Write,
) -> Result<usize> {
    if let Some((start, end)) = rt_range {
        ensure!(
            start.is_finite() && end.is_finite() && start >= 0.0 && start <= end,
            "invalid retention time interval"
        );
    }
    xic_window(mode)?;
    writeln!(out, "rt_seconds,intensity")?;
    chromatogram_points(input, mode, rt_range, &mut |rt, intensity| {
        writeln!(out, "{rt},{intensity}")?;
        Ok(())
    })
}

fn scan_number(id: &str) -> Option<u64> {
    let number = id
        .split_whitespace()
        .rev()
        .find_map(|part| part.strip_prefix("scan="));
    number
        .or_else(|| id.bytes().all(|b| b.is_ascii_digit()).then_some(id))?
        .parse()
        .ok()
}

fn selected_peaks(input: &Path, scan_id: &str, level: Option<u8>) -> Result<Vec<Peak>> {
    ensure!(!scan_id.is_empty(), "scan ID must not be empty");
    let requested_number = scan_id.parse::<u64>().ok();
    let mut peaks = None;
    read_spectra(input, &mut |scan| {
        if peaks.is_some()
            || (scan.id != scan_id
                && requested_number.is_none_or(|n| scan_number(&scan.id) != Some(n)))
        {
            return Ok(());
        }
        if let Some(level) = level {
            ensure!(scan.ms_level == level, "scan {} is not MS{level}", scan.id);
        }
        ensure!(scan.centroided, "scan {} is not centroided", scan.id);
        for peak in &scan.peaks {
            valid_peak(peak).with_context(|| format!("scan {}", scan.id))?;
        }
        ensure!(
            !scan.peaks.is_empty(),
            "scan {} has no centroid peaks",
            scan.id
        );
        peaks = Some(scan.peaks);
        Ok(())
    })?;
    peaks.with_context(|| format!("scan {scan_id:?} not found"))
}

pub(crate) fn spectrum_peaks(input: &Path, scan_id: &str) -> Result<Vec<Peak>> {
    selected_peaks(input, scan_id, None)
}

pub(crate) fn ms2_peaks(input: &Path, scan_id: &str) -> Result<Vec<Peak>> {
    selected_peaks(input, scan_id, Some(2))
}

pub fn spectrum(input: &Path, scan_id: &str, out: &mut dyn Write) -> Result<usize> {
    ensure!(!scan_id.is_empty(), "scan ID must not be empty");
    writeln!(out, "mz,intensity")?;
    let peaks = spectrum_peaks(input, scan_id)?;
    for peak in &peaks {
        writeln!(out, "{},{}", peak.mz, peak.intensity)?;
    }
    Ok(peaks.len())
}
