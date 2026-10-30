use std::path::Path;

use anyhow::Result;

use crate::feature_view;
use crate::inspect::{self, Chromatogram};
use crate::spectrum::Peak;

pub struct ChromPoint {
    pub rt_seconds: f64,
    pub intensity: f64,
}

pub struct PickedFeature {
    pub id: usize,
    pub mz: f64,
    pub rt_seconds: f64,
    pub apex_intensity: f64,
    pub ms2_scan_id: String,
}

pub struct Dataset {
    pub points: Vec<ChromPoint>,
    pub features: Vec<PickedFeature>,
}

pub fn load(raw: &Path, audit: &Path, mode: Chromatogram) -> Result<Dataset> {
    let mut points = Vec::new();
    inspect::chromatogram_points(raw, mode, None, &mut |rt_seconds, intensity| {
        points.push(ChromPoint {
            rt_seconds,
            intensity,
        });
        Ok(())
    })?;
    points.sort_by(|a, b| a.rt_seconds.total_cmp(&b.rt_seconds));

    let xic = inspect::xic_window(mode)?;
    let features = feature_view::accepted_features(audit)?
        .into_iter()
        .filter(|feature| {
            xic.is_none_or(|(mz, tolerance)| (feature.detail.mz - mz).abs() <= tolerance)
        })
        .map(|feature| PickedFeature {
            id: feature.id,
            mz: feature.detail.mz,
            rt_seconds: feature.detail.rt_seconds,
            apex_intensity: feature.detail.apex_intensity,
            ms2_scan_id: feature.detail.ms2_scan_id,
        })
        .collect();
    Ok(Dataset { points, features })
}

pub fn ms2(raw: &Path, id: &str) -> Result<Vec<Peak>> {
    inspect::ms2_peaks(raw, id)
}
