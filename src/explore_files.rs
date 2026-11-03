use std::{
    borrow::Cow,
    collections::HashSet,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MsFile {
    pub raw: PathBuf,
    pub relative: PathBuf,
    pub audit: Option<PathBuf>,
    pub feature_list: Option<PathBuf>,
    pub warning: Option<String>,
}

#[derive(Debug)]
struct OutputMapping {
    dataset_root: PathBuf,
    raw_root: PathBuf,
    output_root: PathBuf,
}

// Use the pipeline's existing raw/output mapping when running from the flock
// checkout. Picoto remains usable alone with adjacent outputs or explicit paths.
fn project_mappings() -> Result<Vec<OutputMapping>> {
    let find_config = |path: &Path| {
        path.ancestors()
            .map(|ancestor| ancestor.join("mato/config"))
            .find(|config| config.is_dir())
    };
    let cwd = std::env::current_dir().context("finding current directory")?;
    let config = find_config(&cwd).or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|executable| find_config(&executable))
    });
    config.map_or(Ok(Vec::new()), |path| load_mappings(&path))
}

fn load_mappings(directory: &Path) -> Result<Vec<OutputMapping>> {
    let mut paths = fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort_unstable();
    let mut mappings = Vec::new();
    for path in paths {
        if path.extension().is_none_or(|extension| extension != "toml") {
            continue;
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        let config: toml::Table = toml::from_str(&text)
            .with_context(|| format!("parsing configuration {}", path.display()))?;
        let Some((dataset, ms_dir, output)) = config.get("dataset").and_then(|dataset| {
            Some((
                dataset.get("root")?.as_str()?,
                dataset.get("ms_dir")?.as_str()?,
                config.get("picoto")?.get("output_dir")?.as_str()?,
            ))
        }) else {
            continue;
        };
        let resolve = |name: &str| {
            let name = Path::new(name);
            if name.is_absolute() {
                name.to_path_buf()
            } else {
                directory.join(name)
            }
        };
        let dataset_root = resolve(dataset);
        let raw_root = if Path::new(ms_dir).is_absolute() {
            resolve(ms_dir)
        } else {
            dataset_root.join(ms_dir)
        };
        mappings.push(OutputMapping {
            dataset_root,
            raw_root,
            output_root: resolve(output),
        });
    }
    Ok(mappings)
}

fn matching_output(raw: &Path, mappings: &[OutputMapping]) -> Result<Option<PathBuf>> {
    if mappings.is_empty() {
        return Ok(None);
    }
    let absolute: Cow<'_, Path> = if raw.is_absolute() {
        Cow::Borrowed(raw)
    } else {
        Cow::Owned(std::env::current_dir()?.join(raw))
    };
    let name = raw.file_stem().context("raw has no file stem")?;
    let mut found = None;
    for mapping in mappings {
        if !absolute.starts_with(&mapping.dataset_root) {
            continue;
        }
        let candidate = if let Ok(relative) = absolute.strip_prefix(&mapping.raw_root) {
            mapping
                .output_root
                .join(suffixed(&relative.with_extension(""), "_features.tsv"))
        } else {
            mapping
                .output_root
                .join(suffixed(Path::new(name), "_features.tsv"))
        };
        if existing_file(&candidate, "audit")?.is_some() {
            if found
                .as_ref()
                .is_some_and(|previous| previous != &candidate)
            {
                bail!(
                    "multiple output directories contain an audit for {}; specify the output directory",
                    raw.display()
                );
            }
            found = Some(candidate);
        }
    }
    Ok(found)
}

fn adjacent_or_mapped(raw: &Path, mappings: &[OutputMapping]) -> Result<Option<PathBuf>> {
    let stem = Path::new(raw.file_stem().context("raw has no file stem")?);
    let adjacent = raw.with_file_name(suffixed(stem, "_features.tsv"));
    if let Some(path) = existing_file(&adjacent, "audit")? {
        return Ok(Some(path));
    }
    matching_output(raw, mappings)
}

fn named_mismatch(path: &Path, expected: &Path, suffix: &str, raw: &Path) -> Option<String> {
    let name = path.file_name()?;
    let named_output = name.to_str()?.ends_with(suffix);
    (named_output && name != expected.as_os_str()).then(|| {
        format!(
            "Warning: {} does not match raw {}; expected {}. Mismatched output ignored",
            name.to_string_lossy(),
            raw.file_name()
                .expect("raw has file name")
                .to_string_lossy(),
            expected.display()
        )
    })
}

/// Locate raw files and their optional, corresponding outputs without reading any MS data.
pub(crate) fn discover_files(
    input: &Path,
    audit: Option<&Path>,
    feature_list: Option<&Path>,
) -> Result<Vec<MsFile>> {
    let needs_mapping = audit.is_none()
        || (input.is_file()
            && audit.is_some_and(|path| {
                input.file_stem().is_some_and(|stem| {
                    named_mismatch(
                        path,
                        &suffixed(Path::new(stem), "_features.tsv"),
                        "_features.tsv",
                        input,
                    )
                    .is_some()
                })
            }));
    let mappings = if needs_mapping {
        project_mappings()?
    } else {
        Vec::new()
    };
    discover_files_with_mappings(input, audit, feature_list, &mappings)
}

fn discover_files_with_mappings(
    input: &Path,
    audit: Option<&Path>,
    feature_list: Option<&Path>,
    mappings: &[OutputMapping],
) -> Result<Vec<MsFile>> {
    let input_kind =
        fs::metadata(input).with_context(|| format!("checking input {}", input.display()))?;
    let single = input_kind.is_file();
    if !single && !input_kind.is_dir() {
        bail!("input is not a file or directory: {}", input.display());
    }
    if single && !crate::is_ms(input) {
        bail!("input is not an mzXML/mzML file: {}", input.display());
    }

    let audit_is_file = supplied_kind(audit, "audit")?;
    let list_is_file = supplied_kind(feature_list, "feature list")?;
    if !single && (audit_is_file || list_is_file) {
        bail!(
            "audit and feature-list files require a single raw input; use directories for directory input"
        );
    }

    let mut discovered = Vec::new();
    if single {
        let name = input.file_name().context("raw input has no file name")?;
        discovered.push((input.to_path_buf(), PathBuf::from(name)));
    } else {
        crate::discover(input, Path::new(""), &mut discovered)?;
    }
    if discovered.is_empty() {
        bail!("no mzXML/mzML files found in {}", input.display());
    }

    let mut stems = HashSet::with_capacity(discovered.len());
    let mut files = Vec::with_capacity(discovered.len());
    for (raw, relative) in discovered {
        let stem = relative.with_extension("");
        if !stems.insert(stem.clone()) {
            bail!(
                "multiple raw files map to the same feature audit stem: {}",
                stem.display()
            );
        }
        let name = Path::new(raw.file_stem().expect("raw has extension"));
        let expected_audit = suffixed(name, "_features.tsv");
        let expected_list = suffixed(name, "_feature_list.tsv");
        let mut warning = None;
        let selected_audit = if audit_is_file {
            let path = audit.expect("validated explicit audit");
            if let Some(message) = named_mismatch(path, &expected_audit, "_features.tsv", &raw) {
                warning = Some(message);
                adjacent_or_mapped(&raw, mappings)?
            } else {
                Some(path.to_path_buf())
            }
        } else if let Some(root) = audit {
            existing_file(&root.join(suffixed(&stem, "_features.tsv")), "audit")?
        } else {
            adjacent_or_mapped(&raw, mappings)?
        };
        let selected_list = if list_is_file {
            let path = feature_list.expect("validated explicit feature list");
            if let Some(message) = named_mismatch(path, &expected_list, "_feature_list.tsv", &raw) {
                warning = Some(match warning {
                    Some(previous) => format!("{previous}; {message}"),
                    None => message,
                });
                None
            } else {
                existing_file(path, "feature list")?
            }
        } else if let Some(root) = feature_list {
            existing_file(
                &root.join(suffixed(&stem, "_feature_list.tsv")),
                "feature list",
            )?
        } else {
            None
        };
        let selected_list = if selected_list.is_none() {
            selected_audit
                .as_deref()
                .map(|path| existing_file(&path.with_file_name(&expected_list), "feature list"))
                .transpose()?
                .flatten()
        } else {
            selected_list
        };
        files.push(MsFile {
            raw,
            relative,
            audit: selected_audit,
            feature_list: selected_list,
            warning,
        });
    }
    Ok(files)
}

fn supplied_kind(path: Option<&Path>, label: &str) -> Result<bool> {
    let Some(path) = path else { return Ok(false) };
    let metadata =
        fs::metadata(path).with_context(|| format!("checking {label} {}", path.display()))?;
    if !metadata.is_file() && !metadata.is_dir() {
        bail!("{label} is not a file or directory: {}", path.display());
    }
    Ok(metadata.is_file())
}

fn existing_file(path: &Path, label: &str) -> Result<Option<PathBuf>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(path.to_path_buf())),
        Ok(_) => bail!("{label} is not a file: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("checking {label} {}", path.display())),
    }
}

fn suffixed(stem: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(stem.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) -> Result<()> {
        fs::create_dir_all(path.parent().expect("test file has parent"))?;
        fs::write(path, [])?;
        Ok(())
    }

    #[test]
    fn nested_raw_files_are_sorted_and_keep_relative_directories() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("raw");
        touch(&root.join("z/third.mzML"))?;
        touch(&root.join("a/second.mzxml"))?;
        touch(&root.join("a/first.MZXML"))?;
        touch(&root.join("ignore.txt"))?;
        let files = discover_files(&root, None, None)?;
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].relative, Path::new("a/first.MZXML"));
        assert_eq!(files[1].relative, Path::new("a/second.mzxml"));
        assert_eq!(files[2].relative, Path::new("z/third.mzML"));
        assert!(
            files
                .iter()
                .all(|file| file.audit.is_none() && file.feature_list.is_none())
        );
        Ok(())
    }

    #[test]
    fn adjacent_audit_is_optional_for_each_raw() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        touch(&root.join("a/one.mzML"))?;
        touch(&root.join("a/one_features.tsv"))?;
        touch(&root.join("b/two.mzXML"))?;
        let files = discover_files(root, None, None)?;
        assert_eq!(
            files[0].audit.as_deref(),
            Some(root.join("a/one_features.tsv").as_path())
        );
        assert!(files[0].feature_list.is_none());
        assert!(files[1].audit.is_none());
        Ok(())
    }

    #[test]
    fn audit_and_feature_list_directories_mirror_raw_relative_paths() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let raw = temp.path().join("raw");
        let audits = temp.path().join("audits");
        let lists = temp.path().join("lists");
        touch(&raw.join("first/sample.mzML"))?;
        touch(&raw.join("second/sample.mzXML"))?;
        touch(&audits.join("first/sample_features.tsv"))?;
        touch(&lists.join("first/sample_feature_list.tsv"))?;
        fs::create_dir_all(&audits)?;
        fs::create_dir_all(&lists)?;
        let files = discover_files(&raw, Some(&audits), Some(&lists))?;
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0].audit.as_deref(),
            Some(audits.join("first/sample_features.tsv").as_path())
        );
        assert_eq!(
            files[0].feature_list.as_deref(),
            Some(lists.join("first/sample_feature_list.tsv").as_path())
        );
        assert!(files[1].audit.is_none());
        assert!(files[1].feature_list.is_none());
        Ok(())
    }

    #[test]
    fn single_raw_accepts_explicit_audit_and_feature_list_files() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let raw = temp.path().join("raw.mzML");
        let audit = temp.path().join("custom.tsv");
        let list = temp.path().join("accepted.tsv");
        touch(&raw)?;
        touch(&audit)?;
        touch(&list)?;
        let files = discover_files(&raw, Some(&audit), Some(&list))?;
        assert_eq!(
            files,
            vec![MsFile {
                raw: raw.clone(),
                relative: PathBuf::from("raw.mzML"),
                audit: Some(audit),
                feature_list: Some(list),
                warning: None,
            }]
        );
        Ok(())
    }

    #[test]
    fn supplied_paths_must_exist_and_single_files_cannot_map_directories() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let raw = temp.path().join("raw.mzML");
        touch(&raw)?;
        let missing = temp.path().join("missing");
        assert!(discover_files(&missing, None, None).is_err());
        assert!(discover_files(&raw, Some(&missing), None).is_err());
        assert!(discover_files(&raw, None, Some(&missing)).is_err());
        let root = temp.path().join("raws");
        touch(&root.join("sample.mzML"))?;
        let audit = temp.path().join("audit.tsv");
        let list = temp.path().join("list.tsv");
        touch(&audit)?;
        touch(&list)?;
        assert!(discover_files(&root, Some(&audit), None).is_err());
        assert!(discover_files(&root, None, Some(&list)).is_err());
        assert!(discover_files(&temp.path().join("other.txt"), None, None).is_err());
        Ok(())
    }

    #[test]
    fn mismatched_named_audit_cannot_attach_another_samples_features() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let raw = temp.path().join("dataset/peak/other.mzML");
        let outputs = temp.path().join("processed");
        let wrong = outputs.join("sample_features.tsv");
        let right = outputs.join("other_features.tsv");
        touch(&raw)?;
        touch(&wrong)?;
        touch(&right)?;
        touch(&outputs.join("other_feature_list.tsv"))?;
        let mappings = [OutputMapping {
            dataset_root: temp.path().join("dataset"),
            raw_root: temp.path().join("dataset/peak"),
            output_root: outputs.clone(),
        }];
        let files = discover_files_with_mappings(&raw, Some(&wrong), None, &mappings)?;
        assert_eq!(files[0].audit.as_deref(), Some(right.as_path()));
        assert!(
            files[0]
                .warning
                .as_deref()
                .unwrap()
                .contains("sample_features.tsv")
        );
        assert!(files[0].warning.as_deref().unwrap().contains("other.mzML"));
        Ok(())
    }

    #[test]
    fn raw_only_finds_accepted_features_under_configured_output_root() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dataset = temp.path().join("dataset");
        let raw = dataset.join("ccms_peak/peak/sample.mzML");
        let outputs = temp.path().join("processed");
        let audit = outputs.join("sample_features.tsv");
        let list = outputs.join("sample_feature_list.tsv");
        touch(&raw)?;
        touch(&audit)?;
        touch(&list)?;
        let mappings = [OutputMapping {
            dataset_root: dataset.clone(),
            raw_root: dataset.join("peak/peak"),
            output_root: outputs,
        }];
        let files = discover_files_with_mappings(&raw, None, None, &mappings)?;
        assert_eq!(files[0].audit.as_deref(), Some(audit.as_path()));
        assert_eq!(files[0].feature_list.as_deref(), Some(list.as_path()));
        assert!(files[0].warning.is_none());
        Ok(())
    }

    #[test]
    fn pipeline_config_maps_nested_raw_files_to_processed_outputs() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dataset = temp.path().join("dataset");
        let raw = dataset.join("peak/peak/sub/sample.mzML");
        let outputs = temp.path().join("processed");
        let audit = outputs.join("sub/sample_features.tsv");
        let list = outputs.join("sub/sample_feature_list.tsv");
        let configs = temp.path().join("mato/config");
        touch(&raw)?;
        touch(&audit)?;
        touch(&list)?;
        fs::create_dir_all(&configs)?;
        fs::write(
            configs.join("massive.toml"),
            format!(
                "# pipeline output mapping\n[dataset]\nroot = {:?}\nms_dir = \"peak/peak\"\n[picoto]\noutput_dir = {:?}\n",
                dataset.display().to_string(),
                outputs.display().to_string(),
            ),
        )?;
        let mappings = load_mappings(&configs)?;
        let files = discover_files_with_mappings(&raw, None, None, &mappings)?;
        assert_eq!(files[0].audit.as_deref(), Some(audit.as_path()));
        assert_eq!(files[0].feature_list.as_deref(), Some(list.as_path()));
        Ok(())
    }

    #[test]
    fn mismatched_feature_list_is_warned_and_matching_list_is_used() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let raw = temp.path().join("sample.mzML");
        let audit = temp.path().join("sample_features.tsv");
        let list = temp.path().join("sample_feature_list.tsv");
        let wrong = temp.path().join("other_feature_list.tsv");
        touch(&raw)?;
        touch(&audit)?;
        touch(&list)?;
        touch(&wrong)?;
        let files = discover_files_with_mappings(&raw, Some(&audit), Some(&wrong), &[])?;
        assert_eq!(files[0].feature_list.as_deref(), Some(list.as_path()));
        assert!(
            files[0]
                .warning
                .as_deref()
                .unwrap()
                .contains("other_feature_list.tsv")
        );
        Ok(())
    }

    #[test]
    fn missing_correct_audit_never_uses_a_named_audit_from_another_run() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let raw = temp.path().join("sample.mzML");
        let wrong = temp.path().join("other_features.tsv");
        touch(&raw)?;
        touch(&wrong)?;
        let files = discover_files_with_mappings(&raw, Some(&wrong), None, &[])?;
        assert!(files[0].audit.is_none());
        assert!(files[0].feature_list.is_none());
        assert!(files[0].warning.is_some());
        Ok(())
    }

    #[test]
    fn duplicate_raw_stems_reject_ambiguous_audit_mapping() -> Result<()> {
        let temp = tempfile::tempdir()?;
        touch(&temp.path().join("a/sample.mzML"))?;
        touch(&temp.path().join("a/sample.mzXML"))?;
        let error = discover_files(temp.path(), None, None).unwrap_err();
        assert!(error.to_string().contains("same feature audit stem"));
        Ok(())
    }
}
