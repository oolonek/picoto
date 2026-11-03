use std::path::Path;

use anyhow::Result;

use crate::feature_view;
use crate::inspect::{self, Chromatogram};

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

pub fn load(raw: &Path, audit: Option<&Path>, mode: Chromatogram) -> Result<Dataset> {
    let points = chromatogram(raw, mode)?;
    let features = audit
        .map(feature_view::accepted_features)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
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

pub(crate) fn chromatogram(raw: &Path, mode: Chromatogram) -> Result<Vec<ChromPoint>> {
    let mut points = Vec::new();
    inspect::chromatogram_points(raw, mode, None, &mut |rt_seconds, intensity| {
        points.push(ChromPoint {
            rt_seconds,
            intensity,
        });
        Ok(())
    })?;
    points.sort_by(|a, b| a.rt_seconds.total_cmp(&b.rt_seconds));
    Ok(points)
}
