# picoto

Pure-Rust, per-file ddMS2 feature picking from **centroided** mzXML or mzML to paired MS1+MS2 SIRIUS-style MGF. It does not require MZmine, an external converter, a database, or cross-file alignment.

```sh
cargo run --release --manifest-path picoto/Cargo.toml -- \
  /path/to/raw.mzXML /path/to/output
cargo run --release --manifest-path picoto/Cargo.toml -- \
  /path/to/raw-directory /path/to/output --jobs 2 --mode both
```

A file or recursively searched directory is accepted. Directory subpaths are mirrored beneath the output directory; only `.mzXML` and `.mzML` files are processed. Output names for `run.mzXML` are `run_sirius.mgf` (default, paired MS1+MS2), `run_ms2.mgf` (`--mode ms2` or `--mode both`), `run_feature_list.tsv` (accepted features only, always), and `run_features.tsv` (acceptance/rejection audit, always). `--mode ms2` produces MS2-only MGF rather than the paired file. Existing outputs for a selected mode are replaced after successful per-file processing; outputs from previously selected modes are left untouched. A file failure does not prevent other files completing, but the batch exits nonzero. Jobs bound simultaneous file processing (`--jobs 4` by default). A terminal progress bar and per-file feature counts are displayed.

The feature list defaults to `feature_id`, `mz`, `rt_seconds`, `peak_height`, `peak_area`, `isotope_peaks`, and `quality_score`. IDs match the accepted audit rows and MGF feature IDs. Height is the apex MS1 intensity; area is the trapezoidal integral of the traced MS1 intensity over retention time (intensity·seconds), **not** a MZmine area. `isotope_peaks` counts observed +1/+2/+3 isotope peaks at the apex (not the monoisotopic peak); `quality_score` is the detector's ranking heuristic, `ln(1 + apex / max(minimum traced intensity, 1)) × (1 + 0.2 × isotope_peaks)`, not a probability or identification confidence. The audit prints this score to three decimals, whereas the feature list retains full precision. MZmine reference tables use minutes for RT and a different peak-picker, so neither coordinates nor areas need match exactly. Add `--metrics data-points,fwhm` to include `ms1_data_points` (number of traced MS1 scans) and `fwhm_seconds` (linearly interpolated width at half height above the trace minimum). Either metric may be requested alone; FWHM is blank if either half-height crossing is absent. Collecting FWHM retains time/intensity samples for active traces, increasing memory use; without it only trace summaries are kept.

```sh
picoto/target/release/picoto /path/to/run.mzXML /path/to/output --metrics data-points,fwhm
```

## Selection and provenance

The mzXML reader streams base64 peak pairs (32/64-bit, uncompressed/zlib); `mzdata` streams mzML peak arrays. Profile or unknown-polarity scans are rejected, not silently centroided or relabeled. MS1 centroids are linked over successive scans using the larger of `--ppm 10` or `--absolute-mz 0.003` Da. Separate chromatographic apices at the same m/z can split at a valley (≤40% of the preceding apex followed by a ≥50% rise). The default gate requires `--min-scans 3`, `--min-height 100000`, an apex-to-minimum intensity ratio of `--min-contrast 3`, a rising and falling trace, and a matched MS2 with at least `--min-ms2-peaks 3` signals above 1% of its base peak. `--max-ms2-rt-offset 20` seconds restricts precursor matching around the feature apex. These are **heuristic defaults**, not a validated chemical identification or signal-to-noise estimate. Check the audit table and adjust for the instrument and samples.

The isotope/correlated-MS1 block contains the monoisotopic apex peak and observed +1/+2/+3 isotope peaks, if present. Missing isotopes do not reject a feature; their count increases the audit `quality_score`, a ranking heuristic (not a probability). The detector retains up to `--max-ms2-scans 8` candidates near the current apex **and** up to eight more from the most recent acquisition window; this protects a later apex from having its MS2 scans evicted by earlier high-scoring scans. Only spectra within `--max-ms2-rt-offset` of the final apex can be exported. Scans can merge only with matching precursor/charge/polarity, collision energy (within 1 eV when recorded), and fragment cosine similarity ≥0.7; fragment alignment uses the larger of 20 ppm or 0.005 Da and never collapses distinct peaks from the same scan. Otherwise the best scan by informative fragment count and intensity is used. Native scan IDs and rejection reasons are recorded in the TSV; MGF `SCANS` and `MERGED_SCANS` contain numeric scan identifiers when available. Collision energy is retained in MGF. The per-file audit includes candidate traces observed in at least two MS1 scans or with an MS2 assignment; isolated single-scan noise is omitted.

Processing uses streaming raw readers, active trace summaries, and a capped MS2 candidate list, not a copy of every input spectrum. Parallel memory still increases with `--jobs`, active chromatograms, accepted features, and audit/output buffers. Files are processed independently: a blank sample is **not** used to filter a biological sample. Feature count alone is not a quality metric; compare audit scores and chromatograms before interpreting biology.

## Terminal inspection and comparison

The static `picoto view chromatogram|spectrum|features|feature` commands use optional [xan](https://github.com/medialab/xan) for terminal plots (`cargo install xan`). `picoto view interactive` uses Ratatui/Crossterm instead, with **no xan requirement**; raw processing and feature selection remain pure Rust. Retention times are seconds. Supply both `--rt-start` and `--rt-end` to limit a static chromatogram; XIC uses the larger of 10 ppm or 0.003 Da unless overridden.

```sh
picoto/target/release/picoto view chromatogram /path/to/raw.mzXML --kind tic
picoto/target/release/picoto view chromatogram /path/to/raw.mzXML --kind bpi
picoto/target/release/picoto view chromatogram /path/to/raw.mzXML --kind xic --mz 343.1548 --rt-start 220 --rt-end 290
picoto/target/release/picoto view spectrum /path/to/raw.mzXML --scan 2193
picoto/target/release/picoto view features /path/to/output/run_features.tsv --plot map
picoto/target/release/picoto view features /path/to/output/run_features.tsv --plot intensity
picoto/target/release/picoto view feature /path/to/raw.mzXML /path/to/output/run_features.tsv FEATURE_ID
picoto/target/release/picoto view interactive /path/to/raw.mzXML /path/to/output/run_features.tsv --kind xic --mz 343.1548
picoto/target/release/picoto view interactive /path/to/raw.mzXML /path/to/output/run_features.tsv --kind tic
```

The feature map shows accepted audit rows by retention time and m/z; the intensity plot shows their MS1 apex intensities. `view feature` prints audit details and plots a ±30 s XIC around its apex plus its first recorded selected MS2 scan. `view spectrum` accepts a numeric scan number or native scan ID. Use the audit `feature_id` for `view feature`, not an MGF ion-block index.

The interactive viewer draws a Braille-resolution TIC, BPI, or XIC (use `--kind bpi` for BPI). Press `+`/`-` or scroll the mouse wheel to zoom in/out around the viewport center or cursor; `r` restores the full range. Click a **◆ accepted-feature marker** to display that feature's selected raw MS2 spectrum beneath the chromatogram. `Tab`/`Shift-Tab` (or right/left arrows) cycle visible features when markers overlap; `q` or Esc exits. A TIC/BPI data point without an accepted audit feature has no unambiguous associated MS2 scan, so clicking the trace alone does not invent one. XIC limits markers to audit features within the requested m/z tolerance. The selected MS2 is read from the raw file on demand (potentially slower for large inputs); interactive memory retains MS1 chromatogram summaries, accepted-feature metadata and at most one selected MS2 spectrum, not every raw scan.

The [six-file overlap plot](evaluation/coordinate-overlap.svg) and [quality-score distribution](evaluation/quality-distribution.svg) live in this repo. Regenerate them and the one-to-one comparison summary with `python3 picoto/evaluation/compare.py --reference-dir /path/to/msdata/processed --picoto-dir /path/to/picoto-output` (requires NumPy, SciPy, matplotlib). The script matches distinct features within `max(0.003 Da, 10 ppm)` and 15 s, with maximum matching cardinality followed by minimum coordinate error. MZmine is a comparator, **not** ground truth: extra picoto features require inspection, especially on blanks.

## Verification and boundaries

CLI tests cover paired chromatographic features and their height/area list, optional traced-point count and FWHM (including an absent crossing), separated apices at one m/z, compatible/incompatible MS2 scans, close-fragment preservation, optional MS2-only output, parallel recursive batches, rejected traces, profile rejection, and later-apex MS2 retention under a tight candidate cap. The six Zenodo mzXML examples under `/media/nvme8tb/isla-gabriela/sapayoa/zenodo-10018590/msdata/raw/` produced 1287, 1091, 195, 223, 544, and 682 paired-feature calls, respectively, in 1.16 seconds wall time (`--jobs 2`, final retention fix). Their MZmine files are illustrative, **not** exact expected results. An earlier centroided mzML example (`mzmlwatcher/202404251608_EB_dbgi_002866_01_01.mzML`) processed 489 features; an earlier 49 MB MassIVE mzML input processed 1028 features in 1.07 seconds with 5,632 KiB peak RSS (`--jobs 1`). These earlier mzML counts/timings were not repeated after the candidate-retention change; none of the runs proves many-thousand-file throughput.

End-to-end SIRIUS import was attempted with the local 6.3.3 CLI; it refused all CLI work because its login refresh token has expired (`invalid_grant`). Paired MGF was generated and inspected, but **SIRIUS ingestion remains unverified** until an authorized SIRIUS session is available. No credentials are needed to run picoto itself.
