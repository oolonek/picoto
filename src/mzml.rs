use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use mzdata::io::DetailLevel;
use mzdata::io::mzml::{MzMLParserError, MzMLParserState, MzMLReader};
use mzdata::meta::MSDataFileMetadata;
use mzdata::spectrum::{IsolationWindowState, MultiLayerSpectrum, ScanPolarity, SignalContinuity};

use crate::spectrum::{Peak, Precursor, Sink, Spectrum};

pub fn read(path: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening mzML {}", path.display()))?;
    // Lazy avoids building a second, eagerly decoded peak list for every scan.
    let mut reader: MzMLReader<File> =
        MzMLReader::with_buffer_capacity_and_detail_level(file, 64 * 1024, DetailLevel::Lazy);
    ensure!(
        matches!(
            reader.state,
            MzMLParserState::SpectrumList | MzMLParserState::Spectrum
        ),
        "invalid mzML metadata in {} (parser state {:?})",
        path.display(),
        reader.state
    );

    let expected = reader.spectrum_count_hint();
    let mut seen = 0_u64;
    loop {
        let mut scan = MultiLayerSpectrum::default();
        match reader.read_into(&mut scan) {
            Ok(_) => {
                sink(convert(scan).with_context(|| format!("reading {}", path.display()))?)?;
                seen += 1;
            }
            Err(MzMLParserError::EOF) if reader.state == MzMLParserState::EOF => break,
            Err(err) => {
                return Err(err).with_context(|| format!("parsing mzML {}", path.display()));
            }
        }
    }
    if let Some(expected) = expected {
        ensure!(
            seen == expected,
            "mzML {}: expected {expected} spectra but read {seen} (file may be truncated)",
            path.display()
        );
    }
    Ok(())
}

fn convert(scan: MultiLayerSpectrum) -> Result<Spectrum> {
    let description = scan.description;
    let id = description.id;
    ensure!(!id.is_empty(), "mzML spectrum has no scan ID");
    ensure!(
        matches!(description.ms_level, 1 | 2),
        "mzML scan {id}: unsupported MS level {} (only MS1/MS2 supported)",
        description.ms_level
    );
    ensure!(
        description.signal_continuity == SignalContinuity::Centroid,
        "mzML scan {id}: unsupported {:?} spectrum; centroided data required",
        description.signal_continuity
    );
    let acquisition = description.acquisition;
    let rt_seconds = acquisition
        .scans
        .first()
        .map(|scan| scan.start_time * 60.0)
        .with_context(|| format!("mzML scan {id}: missing scan start time"))?;
    ensure!(
        rt_seconds.is_finite(),
        "mzML scan {id}: invalid retention time"
    );

    let precursor = if description.ms_level == 2 {
        description
            .precursor
            .first()
            .map(|precursor| {
                let ion = precursor.ions.first();
                let window = &precursor.isolation_window;
                let mz = if window.target > 0.0 {
                    f64::from(window.target)
                } else {
                    ion.map(|ion| ion.mz).unwrap_or_default()
                };
                ensure!(
                    mz.is_finite() && mz > 0.0,
                    "mzML scan {id}: missing precursor m/z"
                );
                let (isolation_lower, isolation_upper) = match window.flags {
                    IsolationWindowState::Complete | IsolationWindowState::Explicit
                        if !window.is_empty() =>
                    {
                        (
                            Some(f64::from(window.lower_bound)),
                            Some(f64::from(window.upper_bound)),
                        )
                    }
                    _ => (None, None),
                };
                Ok(Precursor {
                    mz,
                    charge: ion.and_then(|ion| ion.charge),
                    intensity: ion
                        .and_then(|ion| (ion.intensity != 0.0).then_some(f64::from(ion.intensity))),
                    collision_energy: (precursor.activation.energy != 0.0)
                        .then_some(f64::from(precursor.activation.energy)),
                    isolation_lower,
                    isolation_upper,
                })
            })
            .transpose()?
    } else {
        None
    };

    let arrays = scan
        .arrays
        .with_context(|| format!("mzML scan {id}: missing binary peak arrays"))?;
    let mzs = arrays
        .mzs()
        .with_context(|| format!("mzML scan {id}: decoding m/z array"))?;
    let intensities = arrays
        .intensities()
        .with_context(|| format!("mzML scan {id}: decoding intensity array"))?;
    if mzs.len() != intensities.len() {
        bail!(
            "mzML scan {id}: m/z and intensity array lengths differ ({} vs {})",
            mzs.len(),
            intensities.len()
        );
    }
    let peaks = mzs
        .iter()
        .zip(intensities.iter())
        .map(|(&mz, &intensity)| Peak {
            mz,
            intensity: f64::from(intensity),
        })
        .collect();

    Ok(Spectrum {
        id,
        ms_level: description.ms_level,
        rt_seconds,
        polarity: match description.polarity {
            ScanPolarity::Positive => 1,
            ScanPolarity::Negative => -1,
            ScanPolarity::Unknown => 0,
        },
        centroided: true,
        peaks,
        precursor,
    })
}
