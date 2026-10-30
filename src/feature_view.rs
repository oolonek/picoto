use std::{collections::HashSet, fs::File, io::Write, path::Path};

use anyhow::{Context, Result, bail};

const HEADER: [&str; 14] = [
    "feature_id",
    "mz",
    "rt_seconds",
    "apex_intensity",
    "apex_to_baseline",
    "ms1_scans",
    "area",
    "isotope_peaks",
    "quality_score",
    "status",
    "reason",
    "ms2_scan_ids",
    "ms2_quality",
    "collision_energy",
];

pub enum FeaturePlot {
    Map,
    Intensity,
}

pub struct FeatureDetail {
    pub mz: f64,
    pub rt_seconds: f64,
    pub apex_intensity: f64,
    pub ms2_scan_id: String,
    pub quality_score: f64,
}

pub(crate) struct AcceptedFeature {
    pub id: usize,
    pub detail: FeatureDetail,
}

fn reader(audit: &Path) -> Result<csv::Reader<File>> {
    let file = File::open(audit).with_context(|| format!("opening audit {}", audit.display()))?;
    let mut reader = csv::ReaderBuilder::new().delimiter(b'\t').from_reader(file);
    let headers = reader
        .headers()
        .with_context(|| format!("reading audit header in {}", audit.display()))?;
    if headers.len() != HEADER.len() || !headers.iter().eq(HEADER) {
        bail!("invalid feature audit header in {}", audit.display());
    }
    Ok(reader)
}

fn positive(record: &csv::StringRecord, index: usize, name: &str) -> Result<f64> {
    let value = record[index].parse::<f64>().with_context(|| {
        format!(
            "invalid {name} in feature audit row {}",
            record.position().map_or(0, |p| p.line())
        )
    })?;
    if !value.is_finite() || value <= 0.0 {
        bail!(
            "{name} must be positive and finite in feature audit row {}",
            record.position().map_or(0, |p| p.line())
        );
    }
    Ok(value)
}

fn accepted(record: &csv::StringRecord) -> Result<bool> {
    if record.len() != HEADER.len() {
        bail!(
            "malformed feature audit row {}: expected {} columns",
            record.position().map_or(0, |p| p.line()),
            HEADER.len()
        );
    }
    match &record[9] {
        "accepted" => Ok(true),
        "rejected" => Ok(false),
        status => bail!(
            "invalid feature status {status:?} in audit row {}",
            record.position().map_or(0, |p| p.line())
        ),
    }
}
fn feature_id(record: &csv::StringRecord) -> Result<usize> {
    let id = record[0].parse::<usize>().with_context(|| {
        format!(
            "invalid feature_id in audit row {}",
            record.position().map_or(0, |p| p.line())
        )
    })?;
    if id == 0 {
        bail!(
            "feature_id must be positive in audit row {}",
            record.position().map_or(0, |p| p.line())
        );
    }
    Ok(id)
}

pub fn feature_map(audit: &Path, mode: FeaturePlot, out: &mut dyn Write) -> Result<usize> {
    let mut reader = reader(audit)?;
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    match mode {
        FeaturePlot::Map => writer.write_record(["rt_seconds", "mz"])?,
        FeaturePlot::Intensity => writer.write_record(["rt_seconds", "apex_intensity"])?,
    }
    let mut count = 0;
    for row in reader.records() {
        let row = row.with_context(|| format!("reading feature audit {}", audit.display()))?;
        if !accepted(&row)? {
            continue;
        }
        feature_id(&row)?;
        let rt = positive(&row, 2, "rt_seconds")?;
        let value = match mode {
            FeaturePlot::Map => positive(&row, 1, "mz")?,
            FeaturePlot::Intensity => positive(&row, 3, "apex_intensity")?,
        };
        writer.write_record([rt.to_string(), value.to_string()])?;
        count += 1;
    }
    if count == 0 {
        bail!("no accepted features in audit {}", audit.display());
    }
    writer.flush()?;
    Ok(count)
}

fn parse_detail(row: &csv::StringRecord, id: usize, audit: &Path) -> Result<FeatureDetail> {
    let scan_id = row[11].split(',').next().unwrap_or("");
    if scan_id.trim().is_empty() {
        bail!(
            "missing MS2 scan ID for feature {id} in audit {}",
            audit.display()
        );
    }
    Ok(FeatureDetail {
        mz: positive(row, 1, "mz")?,
        rt_seconds: positive(row, 2, "rt_seconds")?,
        apex_intensity: positive(row, 3, "apex_intensity")?,
        ms2_scan_id: scan_id.to_owned(),
        quality_score: positive(row, 8, "quality_score")?,
    })
}

pub(crate) fn accepted_features(audit: &Path) -> Result<Vec<AcceptedFeature>> {
    let mut reader = reader(audit)?;
    let mut features = Vec::new();
    let mut ids = HashSet::new();
    for row in reader.records() {
        let row = row.with_context(|| format!("reading feature audit {}", audit.display()))?;
        if !accepted(&row)? {
            continue;
        }
        let id = feature_id(&row)?;
        if !ids.insert(id) {
            bail!(
                "duplicate accepted feature_id {id} in audit {}",
                audit.display()
            );
        }
        features.push(AcceptedFeature {
            id,
            detail: parse_detail(&row, id, audit)?,
        });
    }
    Ok(features)
}

pub fn detail(audit: &Path, id: usize) -> Result<FeatureDetail> {
    let mut reader = reader(audit)?;
    let mut found = None;
    for row in reader.records() {
        let row = row.with_context(|| format!("reading feature audit {}", audit.display()))?;
        if !accepted(&row)? || feature_id(&row)? != id {
            continue;
        }
        if found.is_some() {
            bail!(
                "duplicate accepted feature_id {id} in audit {}",
                audit.display()
            );
        }
        found = Some(parse_detail(&row, id, audit)?);
    }
    found.with_context(|| {
        format!(
            "accepted feature_id {id} not found in audit {}",
            audit.display()
        )
    })
}
