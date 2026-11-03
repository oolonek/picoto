use std::{fs, path::Path, process::Command};

use base64::Engine;

fn peaks(points: &[(f64, f64)]) -> String {
    let mut bytes = Vec::new();
    for &(mz, intensity) in points {
        bytes.extend_from_slice(&mz.to_be_bytes());
        bytes.extend_from_slice(&intensity.to_be_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn input(path: &Path, profile: bool, compatible: bool) {
    let mut xml = String::from("<?xml version=\"1.0\"?><mzXML><msRun scanCount=\"7\">");
    for (index, intensity) in [2.0, 30.0, 100.0, 30.0, 2.0].into_iter().enumerate() {
        let time = index + 1;
        xml.push_str(&format!(
            "<scan num=\"{time}\" msLevel=\"1\" centroided=\"{}\" polarity=\"+\" retentionTime=\"PT{time}S\"><peaks precision=\"64\" byteOrder=\"network\" compressionType=\"none\">{}</peaks></scan>",
            if profile { 0 } else { 1 },
            peaks(&[(300.0, intensity), (301.003_355, intensity / 3.0), (555.0, 1.0)])
        ));
        if time == 3 {
            xml.push_str(&format!(
                "<scan num=\"6\" msLevel=\"2\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT3.1S\" collisionEnergy=\"25\"><precursorMz precursorCharge=\"1\" precursorIntensity=\"100\">300.0</precursorMz><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
                peaks(if compatible { &[(80.0, 10.0), (96.965, 12.0), (96.971, 11.0), (120.0, 25.0), (150.0, 50.0), (220.0, 40.0)] } else { &[(80.0, 10.0), (120.0, 25.0), (150.0, 50.0), (220.0, 40.0)] })
            ));
            xml.push_str(&format!(
                "<scan num=\"7\" msLevel=\"2\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT3.2S\" collisionEnergy=\"{}\"><precursorMz precursorCharge=\"1\" precursorIntensity=\"90\">300.0</precursorMz><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
                if compatible { 25 } else { 50 },
                peaks(if compatible { &[(80.0, 10.0), (96.965, 12.0), (96.971, 11.0), (120.0, 25.0), (150.0, 50.0), (220.0, 40.0)] } else { &[(81.0, 10.0), (121.0, 25.0), (151.0, 50.0), (221.0, 40.0)] })
            ));
        }
    }
    xml.push_str("</msRun></mzXML>");
    fs::write(path, xml).unwrap();
}

#[test]
fn paired_mode_picks_chromatographic_feature_and_audits_rejections() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("run.mzXML");
    let output = dir.path().join("out");
    input(&raw, false, false);
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--min-height",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mgf = fs::read_to_string(output.join("run_sirius.mgf")).unwrap();
    assert_eq!(mgf.matches("BEGIN IONS").count(), 2, "{mgf}");
    assert!(mgf.contains("SPECTYPE=CORRELATED MS"));
    assert!(mgf.contains("MSLEVEL=2"));
    assert!(mgf.contains("PEPMASS=300"));
    assert!(
        mgf.contains("SCANS=6"),
        "best scan must retain its raw ID: {mgf}"
    );
    assert!(
        !mgf.contains("SCANS=7"),
        "different collision energies must not merge"
    );
    let table = fs::read_to_string(output.join("run_features.tsv")).unwrap();
    assert!(table.contains("accepted"), "{table}");
    assert!(table.contains("rejected"), "{table}");
    let mut table = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_path(output.join("run_feature_list.tsv"))
        .unwrap();
    assert_eq!(
        table.headers().unwrap().iter().collect::<Vec<_>>(),
        [
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
        ]
    );
    let rows = table.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only accepted features belong in the feature list"
    );
    assert_eq!(&rows[0][0], "1");
    assert_eq!(rows[0][1].parse::<f64>().unwrap(), 300.0);
    assert_eq!(rows[0][2].parse::<f64>().unwrap(), 3.0);
    assert_eq!(rows[0][3].parse::<f64>().unwrap(), 100.0);
    assert_eq!(rows[0][4].parse::<f64>().unwrap(), 162.0);
    assert_eq!(&rows[0][5], "1", "one observed +1 isotope at the apex");
    let quality = rows[0][6].parse::<f64>().unwrap();
    assert!(
        (4.718..4.719).contains(&quality),
        "expected isotope-weighted peak score, got {quality}"
    );
    assert!(!output.join("run_ms2.mgf").exists());
}

#[test]
fn both_mode_writes_optional_ms2_file_and_profile_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("run.mzXML");
    let output = dir.path().join("out");
    input(&raw, false, false);
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--mode",
            "both",
            "--min-height",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let ms2 = fs::read_to_string(output.join("run_ms2.mgf")).unwrap();
    assert_eq!(ms2.matches("BEGIN IONS").count(), 1, "{ms2}");
    assert!(!ms2.contains("MSLEVEL=1"));

    input(&raw, true, false);
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([raw.to_str().unwrap(), output.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("profile"));
}

#[test]
fn compatible_ms2_scans_merge_and_ms2_only_mode_stays_separate() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("run.mzXML");
    let output = dir.path().join("out");
    input(&raw, false, true);
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--mode",
            "ms2",
            "--min-height",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mgf = fs::read_to_string(output.join("run_ms2.mgf")).unwrap();
    assert!(mgf.contains("MERGED_SCANS=6,7"), "{mgf}");
    assert!(
        mgf.contains("96.96500000"),
        "close fragment disappeared: {mgf}"
    );
    assert!(
        mgf.contains("96.97100000"),
        "distinct close fragment disappeared: {mgf}"
    );
    assert_eq!(mgf.matches("BEGIN IONS").count(), 1);
    assert!(!output.join("run_sirius.mgf").exists());
    let table = fs::read_to_string(output.join("run_features.tsv")).unwrap();
    assert!(table.contains("quality_score"));
    assert!(table.contains("6,7"));
}

#[test]
fn recursive_parallel_batch_preserves_nested_file_identity() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("raw");
    let output = dir.path().join("output");
    for polarity in ["pos", "neg"] {
        let folder = source.join(polarity);
        fs::create_dir_all(&folder).unwrap();
        input(&folder.join("run.mzXML"), false, false);
    }
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            source.to_str().unwrap(),
            output.to_str().unwrap(),
            "--jobs",
            "2",
            "--min-height",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for polarity in ["pos", "neg"] {
        let path = output.join(polarity).join("run_sirius.mgf");
        let mgf = fs::read_to_string(path).unwrap();
        assert!(mgf.contains("MSLEVEL=1"));
        assert!(mgf.contains("MSLEVEL=2"));
    }
}

#[test]
fn files_from_processes_only_listed_raw_files_at_their_relative_output_paths() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("raw");
    let output = dir.path().join("output");
    let list = dir.path().join("selected.txt");
    for folder in ["pos", "neg", "skipped"] {
        fs::create_dir_all(source.join(folder)).unwrap();
        input(&source.join(folder).join("run.mzXML"), false, false);
    }
    fs::write(&list, "neg/run.mzXML\npos/run.mzXML\n").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .arg(&source)
        .arg(&output)
        .arg("--files-from")
        .arg(&list)
        .args(["--jobs", "2", "--min-height", "10"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for folder in ["neg", "pos"] {
        let mgf = fs::read_to_string(output.join(folder).join("run_sirius.mgf")).unwrap();
        assert!(mgf.contains("MSLEVEL=2"), "{mgf}");
        assert!(output.join(folder).join("run_features.tsv").exists());
    }
    assert!(!output.join("skipped").exists());
    assert!(!output.join("neg").join("run.mzXML").exists());
}

#[test]
fn files_from_rejects_invalid_lists_before_processing_any_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("raw");
    fs::create_dir_all(&source).unwrap();
    input(&source.join("run.mzXML"), false, false);
    input(&source.join("run.mzML"), false, false);
    fs::create_dir(source.join("folder.mzXML")).unwrap();
    fs::write(source.join("note.txt"), "not raw").unwrap();
    let list = dir.path().join("selected.txt");
    let outside = dir.path().join("outside.mzXML");
    input(&outside, false, false);
    let invalid = [
        ("empty", String::new(), "no mzXML/mzML files"),
        ("absolute", format!("run.mzXML\n{}\n", outside.display()), "relative"),
        ("traversal", "run.mzXML\n../outside.mzXML\n".into(), "parent"),
        ("duplicate", "run.mzXML\nrun.mzXML\n".into(), "duplicate"),
        ("unsupported", "run.mzXML\nnote.txt\n".into(), "mzXML/mzML"),
        ("directory", "run.mzXML\nfolder.mzXML\n".into(), "file"),
        ("missing", "run.mzXML\nmissing.mzXML\n".into(), "file"),
        (
            "collision",
            "run.mzXML\nrun.mzML\n".into(),
            "overwrite one output",
        ),
    ];
    for (case, entries, message) in invalid {
        fs::write(&list, entries).unwrap();
        let output = dir.path().join(format!("out-{case}"));
        let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
            .arg(&source)
            .arg(&output)
            .arg("--files-from")
            .arg(&list)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{case}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(message),
            "{case}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!output.exists(), "{case} processed before validation");
    }
    fs::write(&list, [b"run.mzXML\n".as_slice(), b"\xff.mzXML\n"].concat()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .arg(&source)
        .arg(dir.path().join("out-non-utf8"))
        .arg("--files-from")
        .arg(&list)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("UTF-8"));
    assert!(!dir.path().join("out-non-utf8").exists());
    fs::write(&list, "run.mzXML\n").unwrap();
    let output = dir.path().join("out-file-input");
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .arg(source.join("run.mzXML"))
        .arg(&output)
        .arg("--files-from")
        .arg(&list)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("directory input"));
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn files_from_rejects_symlinks_escaping_the_input_root() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("raw");
    fs::create_dir_all(&source).unwrap();
    input(&source.join("run.mzXML"), false, false);
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    input(&outside.join("escape.mzXML"), false, false);
    symlink(outside.join("escape.mzXML"), source.join("escape.mzXML")).unwrap();
    symlink(&outside, source.join("outside-dir")).unwrap();
    symlink(source.join("run.mzXML"), source.join("alias.mzXML")).unwrap();
    let list = dir.path().join("selected.txt");
    for (case, path) in ["escape.mzXML", "outside-dir/escape.mzXML"]
        .into_iter()
        .enumerate()
    {
        fs::write(&list, format!("run.mzXML\n{path}\n")).unwrap();
        let output = dir.path().join(format!("out-symlink-{case}"));
        let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
            .arg(&source)
            .arg(&output)
            .arg("--files-from")
            .arg(&list)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{path}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("outside"),
            "{path}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!output.exists());
    }
    fs::write(&list, "run.mzXML\nalias.mzXML\n").unwrap();
    let output = dir.path().join("out-alias");
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .arg(&source)
        .arg(&output)
        .arg("--files-from")
        .arg(&list)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("duplicate"));
    assert!(!output.exists());
}

#[test]
fn two_separated_chromatographic_apices_at_one_mz_remain_distinct_features() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("isomers.mzXML");
    let output = dir.path().join("output");
    let mut xml = String::from("<mzXML><msRun>");
    for (index, intensity) in [2.0, 20.0, 80.0, 20.0, 2.0, 20.0, 90.0, 20.0, 2.0]
        .into_iter()
        .enumerate()
    {
        let rt = index + 1;
        xml.push_str(&format!(
            "<scan num=\"{rt}\" msLevel=\"1\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT{rt}S\"><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
            peaks(&[(300.0, intensity), (301.003_355, intensity / 4.0)])
        ));
        if rt == 3 || rt == 7 {
            xml.push_str(&format!(
                "<scan num=\"{}\" msLevel=\"2\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT{rt}.1S\" collisionEnergy=\"25\"><precursorMz precursorCharge=\"1\">300.0</precursorMz><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
                rt + 8, peaks(&[(80.0, 10.0), (120.0, 20.0), (150.0, 30.0)])
            ));
        }
    }
    xml.push_str("</msRun></mzXML>");
    fs::write(&raw, xml).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--min-height",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mgf = fs::read_to_string(output.join("isomers_sirius.mgf")).unwrap();
    assert_eq!(mgf.matches("BEGIN IONS").count(), 4, "{mgf}");
    assert!(mgf.contains("SCANS=11"));
    assert!(mgf.contains("SCANS=15"));
}

#[test]
fn ms2_cap_keeps_lower_quality_scan_near_later_chromatographic_apex() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("late_apex.mzXML");
    let output = dir.path().join("out");
    let mut xml = String::from("<mzXML><msRun>");
    for rt in 1..=25 {
        let intensity = if rt == 12 { 100.0 } else { 20.0 };
        xml.push_str(&format!(
            "<scan num=\"{rt}\" msLevel=\"1\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT{rt}S\"><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
            peaks(&[(300.0, intensity)])
        ));
        if rt <= 8 || rt == 11 {
            let (id, fragments) = if rt == 11 {
                (200, vec![(80.0, 10.0), (120.0, 20.0), (150.0, 30.0)])
            } else {
                (
                    100 + rt,
                    vec![
                        (80.0, 10.0),
                        (100.0, 20.0),
                        (120.0, 30.0),
                        (140.0, 40.0),
                        (160.0, 50.0),
                        (180.0, 60.0),
                    ],
                )
            };
            xml.push_str(&format!(
                "<scan num=\"{id}\" msLevel=\"2\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT{rt}.1S\" collisionEnergy=\"25\"><precursorMz precursorCharge=\"1\">300.0</precursorMz><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
                peaks(&fragments)
            ));
        }
    }
    xml.push_str("</msRun></mzXML>");
    fs::write(&raw, xml).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--min-height",
            "10",
            "--max-ms2-scans",
            "2",
            "--max-ms2-rt-offset",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mgf = fs::read_to_string(output.join("late_apex_sirius.mgf")).unwrap();
    assert_eq!(mgf.matches("BEGIN IONS").count(), 2, "{mgf}");
    assert!(
        mgf.contains("SCANS=200"),
        "near-apex MS2 was evicted: {mgf}"
    );
}

#[test]
fn optional_peak_metrics_follow_ms1_trace_in_ms2_only_mode() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("run.mzXML");
    let output = dir.path().join("out");
    input(&raw, false, false);
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--mode",
            "ms2",
            "--min-height",
            "10",
            "--metrics",
            "data-points,fwhm",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_path(output.join("run_feature_list.tsv"))
        .unwrap();
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        [
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
            "ms1_data_points",
            "fwhm_seconds",
        ]
    );
    let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(&rows[0][7], "5");
    let width = rows[0][8].parse::<f64>().unwrap();
    assert!(
        (width - 1.4).abs() < 1e-10,
        "expected half-height width 1.4 s, got {width}"
    );
}

#[test]
fn fwhm_is_blank_if_the_trace_never_falls_below_half_height() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("unfinished.mzXML");
    let output = dir.path().join("out");
    let mut xml = String::from("<mzXML><msRun>");
    for (index, intensity) in [5.0, 15.0, 100.0, 80.0, 70.0].into_iter().enumerate() {
        let rt = index + 1;
        xml.push_str(&format!(
            "<scan num=\"{rt}\" msLevel=\"1\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT{rt}S\"><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
            peaks(&[(300.0, intensity)])
        ));
        if rt == 3 {
            xml.push_str(&format!(
                "<scan num=\"6\" msLevel=\"2\" centroided=\"1\" polarity=\"+\" retentionTime=\"PT3.1S\"><precursorMz precursorCharge=\"1\">300.0</precursorMz><peaks precision=\"64\" byteOrder=\"network\">{}</peaks></scan>",
                peaks(&[(80.0, 10.0), (120.0, 20.0), (150.0, 30.0)])
            ));
        }
    }
    xml.push_str("</msRun></mzXML>");
    fs::write(&raw, xml).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_picoto"))
        .args([
            raw.to_str().unwrap(),
            output.to_str().unwrap(),
            "--min-height",
            "10",
            "--metrics",
            "fwhm",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_path(output.join("unfinished_feature_list.tsv"))
        .unwrap();
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        [
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
            "fwhm_seconds"
        ]
    );
    let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the MS1/MS2 feature should still be accepted"
    );
    assert_eq!(
        &rows[0][5], "0",
        "no isotope was present in the raw MS1 scans"
    );
    assert_eq!(
        &rows[0][7], "",
        "a missing descending half-height crossing cannot be measured"
    );
}
