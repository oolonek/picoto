use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::explore_data::PickedFeature;

pub struct FeatureRow {
    pub id: usize,
    pub mz: f64,
    pub rt_seconds: f64,
    pub feature_index: usize,
    pub values: Vec<String>,
}

pub struct FeatureList {
    pub headers: Vec<String>,
    pub rows: Vec<FeatureRow>,
}

impl FeatureList {
    pub fn load(
        audit: &Path,
        explicit: Option<&Path>,
        features: &[PickedFeature],
    ) -> Result<Option<Self>> {
        let inferred = || {
            let stem = audit.file_name()?.to_str()?.strip_suffix("_features.tsv")?;
            Some(audit.with_file_name(format!("{stem}_feature_list.tsv")))
        };
        let Some(path) = explicit.map(PathBuf::from).or_else(inferred) else {
            return Ok(None);
        };
        if explicit.is_none() {
            match fs::metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).with_context(|| format!("checking {}", path.display()));
                }
                Ok(metadata) if !metadata.is_file() => {
                    bail!("feature list is not a file: {}", path.display())
                }
                Ok(_) => {}
            }
        }
        let file = File::open(&path)
            .with_context(|| format!("opening feature list {}", path.display()))?;
        let mut reader = csv::ReaderBuilder::new().delimiter(b'\t').from_reader(file);
        let headers = reader
            .headers()
            .with_context(|| format!("reading feature list header in {}", path.display()))?
            .iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        ensure!(
            headers
                .iter()
                .all(|header| !header.is_empty() && seen.insert(header)),
            "duplicate or empty feature-list column in {}",
            path.display()
        );
        let required = |name: &str| {
            headers
                .iter()
                .position(|header| header == name)
                .with_context(|| format!("missing {name} in feature list {}", path.display()))
        };
        let id_col = required("feature_id")?;
        let mz_col = required("mz")?;
        let rt_col = required("rt_seconds")?;
        let accepted = features
            .iter()
            .enumerate()
            .map(|(index, feature)| (feature.id, (index, feature)))
            .collect::<HashMap<_, _>>();
        let mut ids = HashSet::new();
        let mut rows = Vec::with_capacity(features.len());
        for (line, record) in reader.records().enumerate() {
            let record = record.with_context(|| {
                format!("reading feature list {} row {}", path.display(), line + 2)
            })?;
            let id = record[id_col].parse::<usize>().with_context(|| {
                format!("invalid feature_id in {} row {}", path.display(), line + 2)
            })?;
            ensure!(
                ids.insert(id),
                "duplicate feature_id {id} in {}",
                path.display()
            );
            let (feature_index, feature) = accepted.get(&id).with_context(|| {
                format!(
                    "feature_id {id} in {} is absent from the accepted audit",
                    path.display()
                )
            })?;
            let mz = record[mz_col]
                .parse::<f64>()
                .with_context(|| format!("invalid mz in {} row {}", path.display(), line + 2))?;
            let rt = record[rt_col].parse::<f64>().with_context(|| {
                format!("invalid rt_seconds in {} row {}", path.display(), line + 2)
            })?;
            ensure!(
                mz.is_finite()
                    && rt.is_finite()
                    && (mz - feature.mz).abs() <= 0.00001
                    && (rt - feature.rt_seconds).abs() <= 0.001,
                "feature_id {id} coordinates in {} do not match the accepted audit",
                path.display()
            );
            rows.push(FeatureRow {
                id,
                mz,
                rt_seconds: rt,
                feature_index: *feature_index,
                values: record.iter().map(str::to_owned).collect(),
            });
        }
        ensure!(
            rows.len() == features.len(),
            "feature list {} has {} rows but audit has {} accepted features",
            path.display(),
            rows.len(),
            features.len()
        );
        Ok(Some(Self { headers, rows }))
    }

    pub fn filter(&self, query: &str) -> Result<Vec<usize>> {
        let query = query.trim();
        let (column, term) = if let Some((name, term)) = query.split_once(':') {
            let column = self
                .headers
                .iter()
                .position(|header| header.eq_ignore_ascii_case(name.trim()))
                .with_context(|| format!("unknown feature-list column {name:?}"))?;
            (Some(column), term.trim())
        } else {
            (None, query)
        };
        Ok(self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let matches = |value: &str| {
                    term.is_empty()
                        || value
                            .as_bytes()
                            .windows(term.len())
                            .any(|window| window.eq_ignore_ascii_case(term.as_bytes()))
                };
                if column.map_or_else(
                    || row.values.iter().any(|value| matches(value)),
                    |column| matches(&row.values[column]),
                ) {
                    Some(index)
                } else {
                    None
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_every_column_including_optional_metrics() -> Result<()> {
        let list = FeatureList {
            headers: [
                "feature_id",
                "mz",
                "rt_seconds",
                "quality_score",
                "ms1_data_points",
                "fwhm_seconds",
            ]
            .map(str::to_owned)
            .to_vec(),
            rows: vec![
                FeatureRow {
                    id: 1,
                    mz: 100.0,
                    rt_seconds: 12.0,
                    feature_index: 0,
                    values: ["1", "100", "12", "6.41", "49", "0.288"]
                        .map(str::to_owned)
                        .to_vec(),
                },
                FeatureRow {
                    id: 2,
                    mz: 200.0,
                    rt_seconds: 30.0,
                    feature_index: 1,
                    values: ["2", "200", "30", "4.7", "5", ""]
                        .map(str::to_owned)
                        .to_vec(),
                },
            ],
        };
        assert_eq!(list.filter("49")?, vec![0]);
        assert_eq!(list.filter("MS1_DATA_POINTS:49")?, vec![0]);
        assert_eq!(list.filter("quality_score:4.7")?, vec![1]);
        assert_eq!(list.filter("fwhm_seconds:0.28")?, vec![0]);
        assert_eq!(list.filter("mz:200")?, vec![1]);
        assert!(list.filter("unknown:5").is_err());
        Ok(())
    }

    #[test]
    fn rejects_stale_list_coordinates_before_linking_ms2() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let audit = directory.path().join("sample_features.tsv");
        let list_path = directory.path().join("sample_feature_list.tsv");
        let accepted = vec![
            PickedFeature {
                id: 2,
                mz: 200.0,
                rt_seconds: 30.0,
                apex_intensity: 10.0,
                ms2_scan_id: "scan=8".into(),
            },
            PickedFeature {
                id: 1,
                mz: 100.0,
                rt_seconds: 12.492,
                apex_intensity: 20.0,
                ms2_scan_id: "scan=6".into(),
            },
        ];
        assert!(FeatureList::load(&audit, None, &accepted)?.is_none());
        fs::write(
            &list_path,
            "feature_id\tmz\trt_seconds\tquality_score\n1\t100.0000002\t12.4921\t5.4\n2\t200\t30\t4.2\n",
        )?;
        let list = FeatureList::load(&audit, None, &accepted)?.context("list not discovered")?;
        assert_eq!(list.rows[0].feature_index, 1);
        fs::write(
            &list_path,
            "feature_id\tmz\trt_seconds\tquality_score\n1\t101\t12.4921\t5.4\n2\t200\t30\t4.2\n",
        )?;
        assert!(
            format!(
                "{:#}",
                FeatureList::load(&audit, None, &accepted)
                    .err()
                    .context("stale coordinates accepted")?
            )
            .contains("do not match")
        );
        Ok(())
    }
}
