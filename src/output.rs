use std::{io::Write, path::Path};

use anyhow::Result;

use crate::{feature::Feature, spectrum::Peak};

fn scan_number(id: &str) -> Option<&str> {
    let number = if let Some(offset) = id.rfind("scan=") {
        &id[offset + 5..]
    } else {
        id
    };
    let digits = number.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && (id.contains("scan=") || digits == id.len())).then_some(&number[..digits])
}

fn spectrum(
    writer: &mut dyn Write,
    feature: &Feature,
    filename: &str,
    level: u8,
    peaks: &[Peak],
) -> Result<()> {
    writeln!(writer, "BEGIN IONS")?;
    writeln!(writer, "FEATURE_ID={}", feature.id)?;
    writeln!(writer, "TITLE={filename}:feature_{}:MS{level}", feature.id)?;
    writeln!(writer, "PEPMASS={:.8}", feature.mz)?;
    writeln!(
        writer,
        "CHARGE={}{}",
        feature.charge,
        if feature.polarity < 0 { '-' } else { '+' }
    )?;
    writeln!(writer, "RTINSECONDS={:.4}", feature.rt_seconds)?;
    if level == 1 {
        writeln!(writer, "SPECTYPE=CORRELATED MS")?;
    }
    writeln!(writer, "MSLEVEL={level}")?;
    writeln!(writer, "FILENAME={filename}")?;
    if level == 1 {
        writeln!(writer, "SCANS=-1")?;
    } else {
        // mzML native IDs can contain spaces and key/value pairs. MGF SCANS is numeric;
        // full IDs remain in the per-file audit table.
        if let Some(scan) = scan_number(&feature.scan_ids[0]) {
            writeln!(writer, "SCANS={scan}")?;
        }
        if feature.scan_ids.len() > 1 {
            let scans: Option<Vec<_>> = feature.scan_ids.iter().map(|id| scan_number(id)).collect();
            if let Some(scans) = scans {
                writeln!(writer, "MERGED_SCANS={}", scans.join(","))?;
            }
        }
        if let Some(energy) = feature.collision_energy {
            writeln!(writer, "COLLISIONENERGY={energy}")?;
        }
    }
    for peak in peaks {
        writeln!(writer, "{:.8} {:.3}", peak.mz, peak.intensity)?;
    }
    writeln!(writer, "END IONS\n")?;
    Ok(())
}

pub fn write_mgf(
    writer: &mut dyn Write,
    features: &[Feature],
    input: &Path,
    include_ms1: bool,
) -> Result<()> {
    let filename = input
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    for feature in features {
        if include_ms1 {
            spectrum(writer, feature, filename, 1, &feature.ms1)?;
        }
        spectrum(writer, feature, filename, 2, &feature.ms2)?;
    }
    Ok(())
}
