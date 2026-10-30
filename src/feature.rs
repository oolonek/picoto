use std::{collections::HashMap, io::Write, path::Path};

use anyhow::{Context, Result, bail};

use crate::spectrum::{self, Peak, Spectrum};

const BIN_WIDTH: f64 = 0.01;

#[derive(Clone, Debug)]
pub struct Settings {
    pub min_height: f64,
    pub min_scans: usize,
    pub min_contrast: f64,
    pub ppm: f64,
    pub absolute_mz: f64,
    pub min_ms2_peaks: usize,
    pub max_ms2_rt_offset: f64,
    pub max_ms2_scans: usize,
    pub compute_fwhm: bool,
}

impl Settings {
    pub fn tolerance(&self, mz: f64) -> f64 {
        self.absolute_mz.max(mz * self.ppm * 1e-6)
    }
}

#[derive(Debug)]
struct Trace {
    mz: f64,
    bin: i64,
    last_rt: f64,
    last_scan: usize,
    scans: usize,
    apex: f64,
    apex_rt: f64,
    first_intensity: f64,
    last_intensity: f64,
    baseline: f64,
    area: f64,
    isotopes: [Peak; 4],
    isotope_count: usize,
    ms2: Vec<Spectrum>,
    fwhm_samples: Option<Vec<(f64, f64)>>,
    apex_sample_index: usize,
}

impl Trace {
    fn new(peak: Peak, scan: usize, rt: f64, peaks: &[Peak], compute_fwhm: bool) -> Self {
        let (isotopes, isotope_count) = isotope_envelope(peaks, peak);
        Self {
            mz: peak.mz,
            bin: (peak.mz / BIN_WIDTH).round() as i64,
            last_rt: rt,
            last_scan: scan,
            scans: 1,
            apex: peak.intensity,
            apex_rt: rt,
            first_intensity: peak.intensity,
            last_intensity: peak.intensity,
            baseline: peak.intensity,
            area: 0.0,
            isotopes,
            isotope_count,
            ms2: Vec::new(),
            fwhm_samples: compute_fwhm.then(|| vec![(rt, peak.intensity)]),
            apex_sample_index: 0,
        }
    }

    fn add(&mut self, peak: Peak, scan: usize, rt: f64, peaks: &[Peak], settings: &Settings) {
        self.area += (rt - self.last_rt).max(0.0) * (peak.intensity + self.last_intensity) / 2.0;
        self.mz += (peak.mz - self.mz) / (self.scans + 1) as f64;
        self.scans += 1;
        self.last_scan = scan;
        self.last_rt = rt;
        self.last_intensity = peak.intensity;
        if let Some(samples) = &mut self.fwhm_samples {
            samples.push((rt, peak.intensity));
        }
        self.baseline = self.baseline.min(peak.intensity);
        if peak.intensity > self.apex {
            self.apex = peak.intensity;
            self.apex_rt = rt;
            self.apex_sample_index = self.scans - 1;
            (self.isotopes, self.isotope_count) = isotope_envelope(peaks, peak);
            self.trim_ms2(settings, rt);
        }
    }

    fn trim_ms2(&mut self, settings: &Settings, latest_rt: f64) {
        // A later apex can invalidate the best spectra seen so far. Reserve
        // capacity for both the known apex and the most recent acquisition window.
        if self.ms2.len() <= settings.max_ms2_scans {
            return;
        }
        self.ms2
            .sort_unstable_by(|a, b| ms2_quality(b).total_cmp(&ms2_quality(a)));
        let mut near_apex = 0;
        let mut recent = 0;
        self.ms2.retain(|scan| {
            let near = (scan.rt_seconds - self.apex_rt).abs() <= settings.max_ms2_rt_offset;
            let current = scan.rt_seconds >= latest_rt - settings.max_ms2_rt_offset;
            let keep = (near && near_apex < settings.max_ms2_scans)
                || (current && recent < settings.max_ms2_scans);
            if keep {
                near_apex += usize::from(near);
                recent += usize::from(current);
            }
            keep
        });
    }

    fn new_peak_after_valley(&self, intensity: f64) -> bool {
        self.scans >= 4
            && self.last_rt > self.apex_rt
            && self.last_intensity <= self.apex * 0.4
            && intensity > self.last_intensity * 1.5
    }

    fn contrast(&self) -> f64 {
        self.apex / self.baseline.max(1.0)
    }

    fn fwhm_seconds(&self) -> Option<f64> {
        let samples = self.fwhm_samples.as_ref()?;
        let threshold = self.baseline + (self.apex - self.baseline) / 2.0;
        let left = samples[..=self.apex_sample_index]
            .windows(2)
            .rev()
            .find(|pair| pair[0].1 <= threshold && pair[1].1 >= threshold)?;
        let right = samples[self.apex_sample_index..]
            .windows(2)
            .find(|pair| pair[0].1 >= threshold && pair[1].1 <= threshold)?;
        let crossing = |pair: &[(f64, f64)], nearer_apex: usize| {
            if pair[0].1 == pair[1].1 {
                pair[nearer_apex].0
            } else {
                pair[0].0
                    + (pair[1].0 - pair[0].0) * (threshold - pair[0].1) / (pair[1].1 - pair[0].1)
            }
        };
        Some(crossing(right, 0) - crossing(left, 1))
    }
}

fn nearest(peaks: &[Peak], mz: f64, tolerance: f64) -> Option<Peak> {
    let start = peaks.partition_point(|p| p.mz < mz - tolerance);
    peaks[start..]
        .iter()
        .take_while(|p| p.mz <= mz + tolerance)
        .copied()
        .min_by(|a, b| (a.mz - mz).abs().total_cmp(&(b.mz - mz).abs()))
}

fn isotope_envelope(peaks: &[Peak], mono: Peak) -> ([Peak; 4], usize) {
    let mut result = [mono; 4];
    let mut count = 1;
    for n in 1..=3 {
        let target = mono.mz + n as f64 * 1.003_355;
        if let Some(peak) = nearest(peaks, target, (target * 10e-6).max(0.004)) {
            if peak.intensity > 0.0 {
                result[count] = peak;
                count += 1;
            }
        }
    }
    (result, count)
}

fn ms2_quality(scan: &Spectrum) -> f64 {
    let max = scan.peaks.iter().map(|p| p.intensity).fold(0.0, f64::max);
    let informative = scan
        .peaks
        .iter()
        .filter(|p| p.intensity >= max * 0.01 && p.intensity > 0.0)
        .count();
    informative as f64 * 10.0
        + scan
            .precursor
            .as_ref()
            .and_then(|p| p.intensity)
            .unwrap_or_else(|| scan.peaks.iter().map(|p| p.intensity).sum::<f64>())
            .max(0.0)
            .ln_1p()
}

fn informative_peaks(scan: &Spectrum) -> usize {
    let max = scan.peaks.iter().map(|p| p.intensity).fold(0.0, f64::max);
    scan.peaks
        .iter()
        .filter(|p| p.intensity > 0.0 && p.intensity >= max * 0.01)
        .count()
}
fn fragment_tolerance(mz: f64) -> f64 {
    (mz * 20e-6).max(0.005)
}

fn cosine(a: &[Peak], b: &[Peak]) -> f64 {
    let mut i = 0;
    let mut j = 0;
    let mut dot = 0.0;
    while i < a.len() && j < b.len() {
        let tolerance = fragment_tolerance(a[i].mz);
        if (a[i].mz - b[j].mz).abs() <= tolerance {
            dot += a[i].intensity.sqrt() * b[j].intensity.sqrt();
            i += 1;
            j += 1;
        } else if a[i].mz < b[j].mz {
            i += 1;
        } else {
            j += 1;
        }
    }
    let norm_a: f64 = a.iter().map(|p| p.intensity.max(0.0)).sum();
    let norm_b: f64 = b.iter().map(|p| p.intensity.max(0.0)).sum();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b).sqrt()
    }
}

fn compatible(a: &Spectrum, b: &Spectrum, settings: &Settings) -> bool {
    let (Some(pa), Some(pb)) = (&a.precursor, &b.precursor) else {
        return false;
    };
    a.polarity == b.polarity
        && pa.charge == pb.charge
        && (pa.mz - pb.mz).abs() <= settings.tolerance(pa.mz)
        && match (pa.collision_energy, pb.collision_energy) {
            (Some(a), Some(b)) => (a - b).abs() <= 1.0,
            (None, None) => true,
            _ => false,
        }
        && cosine(&a.peaks, &b.peaks) >= 0.7
}

fn merge_peaks(scans: &[&Spectrum]) -> Vec<Peak> {
    if scans.len() == 1 {
        return scans[0].peaks.clone();
    }
    let mut merged = scans[0].peaks.clone();
    merged.sort_unstable_by(|a, b| a.mz.total_cmp(&b.mz));
    for scan in scans.iter().skip(1) {
        let existing = merged.len();
        let mut used = vec![false; existing];
        let mut updates = Vec::new();
        let mut additions = Vec::new();
        for &peak in &scan.peaks {
            let tolerance = fragment_tolerance(peak.mz);
            let start = merged[..existing].partition_point(|p| p.mz < peak.mz - tolerance);
            let best = (start..existing)
                .take_while(|&index| merged[index].mz <= peak.mz + tolerance)
                .filter(|&index| !used[index])
                .min_by(|&a, &b| {
                    (merged[a].mz - peak.mz)
                        .abs()
                        .total_cmp(&(merged[b].mz - peak.mz).abs())
                });
            if let Some(index) = best {
                used[index] = true;
                updates.push((index, peak));
            } else {
                additions.push(peak);
            }
        }
        for (index, peak) in updates {
            let target = &mut merged[index];
            let sum = target.intensity + peak.intensity;
            if sum > 0.0 {
                target.mz += (peak.mz - target.mz) * peak.intensity / sum;
            }
            target.intensity = sum;
        }
        merged.extend(additions);
        merged.sort_unstable_by(|a, b| a.mz.total_cmp(&b.mz));
    }
    merged
}

pub struct Feature {
    pub id: usize,
    pub mz: f64,
    pub rt_seconds: f64,
    pub peak_height: f64,
    pub peak_area: f64,
    pub isotope_peaks: usize,
    pub quality_score: f64,
    pub ms1_data_points: usize,
    pub fwhm_seconds: Option<f64>,
    pub ms1: Vec<Peak>,
    pub ms2: Vec<Peak>,
    pub scan_ids: Vec<String>,
    pub charge: i32,
    pub polarity: i8,
    pub collision_energy: Option<f64>,
}

struct Detector<'a> {
    settings: &'a Settings,
    table: &'a mut dyn Write,
    active: Vec<Option<Trace>>,
    free: Vec<usize>,
    bins: HashMap<i64, Vec<usize>>,
    scan_index: usize,
    features: Vec<Feature>,
    polarity: Option<i8>,
}

impl Detector<'_> {
    fn finalize(&mut self, index: usize) -> Result<()> {
        let mut trace = self.active[index].take().expect("active trace");
        if let Some(slots) = self.bins.get_mut(&trace.bin) {
            slots.retain(|&id| id != index);
            if slots.is_empty() {
                self.bins.remove(&trace.bin);
            }
        }
        self.free.push(index);
        trace.ms2.retain(|scan| {
            (scan.rt_seconds - trace.apex_rt).abs() <= self.settings.max_ms2_rt_offset
        });
        if trace.scans < 2 && trace.ms2.is_empty() {
            return Ok(());
        }
        let reason = if trace.scans < self.settings.min_scans {
            "too_few_ms1_scans"
        } else if trace.apex < self.settings.min_height {
            "low_ms1_height"
        } else if trace.contrast() < self.settings.min_contrast
            || trace.apex <= trace.first_intensity
            || trace.apex <= trace.last_intensity
        {
            "no_chromatographic_peak"
        } else if trace.ms2.is_empty() {
            "no_matched_ms2"
        } else {
            "accepted"
        };
        let contrast = trace.contrast();
        let isotope_peaks = trace.isotope_count.saturating_sub(1);
        let score = contrast.ln_1p() * (1.0 + isotope_peaks as f64 * 0.2);
        let fwhm_seconds = if reason == "accepted" {
            trace.fwhm_seconds()
        } else {
            None
        };
        let mut candidates = trace.ms2;
        let mut selected = Vec::new();
        if reason == "accepted" {
            candidates.sort_unstable_by(|a, b| {
                ms2_quality(b).total_cmp(&ms2_quality(a)).then_with(|| {
                    (a.rt_seconds - trace.apex_rt)
                        .abs()
                        .total_cmp(&(b.rt_seconds - trace.apex_rt).abs())
                })
            });
            let best = &candidates[0];
            selected.push(best);
            for next in candidates.iter().skip(1) {
                if compatible(best, next, self.settings) {
                    selected.push(next);
                }
            }
            let id = self.features.len() + 1;
            self.features.push(Feature {
                id,
                mz: trace.mz,
                peak_height: trace.apex,
                peak_area: trace.area,
                isotope_peaks,
                quality_score: score,
                ms1_data_points: trace.scans,
                fwhm_seconds,
                rt_seconds: trace.apex_rt,
                ms1: trace.isotopes[..trace.isotope_count].to_vec(),
                ms2: merge_peaks(&selected),
                scan_ids: selected.iter().map(|s| s.id.clone()).collect(),
                charge: best
                    .precursor
                    .as_ref()
                    .and_then(|p| p.charge)
                    .unwrap_or(1)
                    .abs()
                    .max(1),
                polarity: best.polarity,
                collision_energy: best.precursor.as_ref().and_then(|p| p.collision_energy),
            });
        }
        let ids = selected
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let energy = selected
            .first()
            .and_then(|s| s.precursor.as_ref())
            .and_then(|p| p.collision_energy);
        let ms2_score = selected.first().map(|s| ms2_quality(s));
        writeln!(
            self.table,
            "{}\t{:.6}\t{:.3}\t{:.1}\t{:.2}\t{}\t{:.1}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}",
            self.features
                .last()
                .filter(|f| f.mz == trace.mz && reason == "accepted")
                .map(|f| f.id.to_string())
                .unwrap_or_default(),
            trace.mz,
            trace.apex_rt,
            trace.apex,
            contrast,
            trace.scans,
            trace.area,
            isotope_peaks,
            score,
            if reason == "accepted" {
                "accepted"
            } else {
                "rejected"
            },
            reason,
            ids,
            ms2_score.map(|s| format!("{s:.2}")).unwrap_or_default(),
            energy.map(|e| e.to_string()).unwrap_or_default()
        )?;
        Ok(())
    }

    fn push(&mut self, mut spectrum: Spectrum) -> Result<()> {
        if !spectrum.centroided {
            bail!(
                "profile spectrum {} is not supported; provide centroided mzML/mzXML",
                spectrum.id
            );
        }
        if !spectrum.rt_seconds.is_finite() {
            bail!("invalid retention time in spectrum {}", spectrum.id);
        }
        if spectrum.polarity == 0 {
            bail!("unknown polarity in spectrum {}", spectrum.id);
        }
        if let Some(polarity) = self.polarity {
            if spectrum.polarity != polarity {
                bail!("mixed polarities in one raw file");
            }
        } else {
            self.polarity = Some(spectrum.polarity);
        }
        spectrum.peaks.retain(|p| {
            p.mz.is_finite() && p.mz > 0.0 && p.intensity.is_finite() && p.intensity > 0.0
        });
        match spectrum.ms_level {
            1 => self.push_ms1(spectrum),
            2 => self.push_ms2(spectrum),
            _ => Ok(()),
        }
    }

    fn push_ms1(&mut self, mut spectrum: Spectrum) -> Result<()> {
        self.scan_index += 1;
        for index in 0..self.active.len() {
            if self.active[index]
                .as_ref()
                .is_some_and(|trace| self.scan_index - trace.last_scan > 2)
            {
                self.finalize(index)?;
            }
        }
        if !spectrum.peaks.windows(2).all(|w| w[0].mz <= w[1].mz) {
            spectrum
                .peaks
                .sort_unstable_by(|a, b| a.mz.total_cmp(&b.mz));
        }
        for &peak in &spectrum.peaks {
            let bin = (peak.mz / BIN_WIDTH).round() as i64;
            let radius = (self.settings.tolerance(peak.mz) / BIN_WIDTH).ceil() as i64 + 2;
            let mut best: Option<(usize, f64)> = None;
            for key in (bin - radius)..=(bin + radius) {
                if let Some(indices) = self.bins.get(&key) {
                    for &idx in indices {
                        if let Some(trace) = self.active[idx].as_ref() {
                            let error = (trace.mz - peak.mz).abs();
                            if trace.last_scan != self.scan_index
                                && error <= self.settings.tolerance(peak.mz)
                                && best.is_none_or(|(_, old)| error < old)
                            {
                                best = Some((idx, error));
                            }
                        }
                    }
                }
            }
            if let Some((idx, _)) = best {
                if self.active[idx]
                    .as_ref()
                    .unwrap()
                    .new_peak_after_valley(peak.intensity)
                {
                    self.finalize(idx)?;
                    let idx = self.free.pop().expect("just finalized trace");
                    self.active[idx] = Some(Trace::new(
                        peak,
                        self.scan_index,
                        spectrum.rt_seconds,
                        &spectrum.peaks,
                        self.settings.compute_fwhm,
                    ));
                    self.bins.entry(bin).or_default().push(idx);
                } else {
                    self.active[idx].as_mut().unwrap().add(
                        peak,
                        self.scan_index,
                        spectrum.rt_seconds,
                        &spectrum.peaks,
                        self.settings,
                    );
                }
            } else {
                let idx = self.free.pop().unwrap_or_else(|| {
                    self.active.push(None);
                    self.active.len() - 1
                });
                self.active[idx] = Some(Trace::new(
                    peak,
                    self.scan_index,
                    spectrum.rt_seconds,
                    &spectrum.peaks,
                    self.settings.compute_fwhm,
                ));
                self.bins.entry(bin).or_default().push(idx);
            }
        }
        Ok(())
    }

    fn push_ms2(&mut self, spectrum: Spectrum) -> Result<()> {
        let Some(precursor) = spectrum.precursor.as_ref() else {
            return Ok(());
        };
        if informative_peaks(&spectrum) < self.settings.min_ms2_peaks {
            return Ok(());
        }
        let target = precursor.mz;
        let bin = (target / BIN_WIDTH).round() as i64;
        let radius = (self.settings.tolerance(target) / BIN_WIDTH).ceil() as i64 + 2;
        let mut best: Option<(usize, f64)> = None;
        for key in (bin - radius)..=(bin + radius) {
            if let Some(indices) = self.bins.get(&key) {
                for &idx in indices {
                    if let Some(trace) = self.active[idx].as_ref() {
                        if precursor
                            .isolation_lower
                            .is_some_and(|lower| trace.mz < lower)
                            || precursor
                                .isolation_upper
                                .is_some_and(|upper| trace.mz > upper)
                        {
                            continue;
                        }
                        let error = (trace.mz - target).abs();
                        if error <= self.settings.tolerance(target)
                            && (trace.last_rt - spectrum.rt_seconds).abs()
                                <= self.settings.max_ms2_rt_offset
                            && best.is_none_or(|(_, old)| error < old)
                        {
                            best = Some((idx, error));
                        }
                    }
                }
            }
        }
        if let Some((idx, _)) = best {
            let trace = self.active[idx].as_mut().unwrap();
            let rt = spectrum.rt_seconds;
            trace.ms2.push(spectrum);
            trace.trim_ms2(self.settings, rt);
        }
        Ok(())
    }
}

pub fn detect(path: &Path, settings: &Settings, table: &mut dyn Write) -> Result<Vec<Feature>> {
    writeln!(
        table,
        "feature_id\tmz\trt_seconds\tapex_intensity\tapex_to_baseline\tms1_scans\tarea\tisotope_peaks\tquality_score\tstatus\treason\tms2_scan_ids\tms2_quality\tcollision_energy"
    )?;
    let mut detector = Detector {
        settings,
        table,
        active: Vec::new(),
        free: Vec::new(),
        bins: HashMap::new(),
        scan_index: 0,
        features: Vec::new(),
        polarity: None,
    };
    spectrum::read_spectra(path, &mut |scan| detector.push(scan))
        .with_context(|| format!("reading {}", path.display()))?;
    for index in 0..detector.active.len() {
        if detector.active[index].is_some() {
            detector.finalize(index)?;
        }
    }
    Ok(detector.features)
}
