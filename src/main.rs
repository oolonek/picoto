mod explore;
mod explore_data;
mod feature;
mod feature_table;
mod feature_view;
mod inspect;
mod mzml;
mod mzxml;
mod output;
mod spectrum;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
};

use crate::feature::Settings;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputMode {
    #[default]
    Paired,
    Ms2,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ExtraMetric {
    DataPoints,
    Fwhm,
}

#[derive(Debug, Parser)]
#[command(
    about = "Detect chromatographic ddMS2 features in centroided mzXML/mzML, export SIRIUS-style MGF"
)]
struct Cli {
    /// Single mzXML/mzML file, or a directory searched recursively
    input: PathBuf,
    /// Root for per-file MGF, accepted feature-list TSV, and feature audit TSV outputs
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = OutputMode::Paired)]
    mode: OutputMode,
    #[arg(long, default_value_t = 4)]
    jobs: usize,
    #[arg(long, default_value_t = 100_000.0)]
    min_height: f64,
    #[arg(long, default_value_t = 3)]
    min_scans: usize,
    /// Minimum MS1 apex/baseline intensity ratio
    #[arg(long, default_value_t = 3.0)]
    min_contrast: f64,
    #[arg(long, default_value_t = 10.0)]
    ppm: f64,
    #[arg(long, default_value_t = 0.003)]
    absolute_mz: f64,
    #[arg(long, default_value_t = 3)]
    min_ms2_peaks: usize,
    #[arg(long, default_value_t = 20.0)]
    max_ms2_rt_offset: f64,
    #[arg(long, default_value_t = 8)]
    max_ms2_scans: usize,
    /// Additional accepted-feature columns; e.g. --metrics data-points,fwhm
    #[arg(long, value_enum, value_delimiter = ',')]
    metrics: Vec<ExtraMetric>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ChromatogramKind {
    #[default]
    Tic,
    Bpi,
    Xic,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum FeaturePlotKind {
    #[default]
    Map,
    Intensity,
}

#[derive(Parser)]
#[command(
    about = "Inspect chromatograms, spectra, and picked features in terminal plots or an interactive viewer"
)]
struct ViewCli {
    #[command(subcommand)]
    command: ViewCommand,
}

#[derive(clap::Subcommand)]
enum ViewCommand {
    /// Plot MS1 total ion, base peak, or extracted ion chromatogram
    Chromatogram {
        input: PathBuf,
        #[arg(long, value_enum, default_value_t = ChromatogramKind::Tic)]
        kind: ChromatogramKind,
        #[arg(long)]
        mz: Option<f64>,
        #[arg(long, default_value_t = 10.0)]
        ppm: f64,
        #[arg(long, default_value_t = 0.003)]
        absolute_mz: f64,
        #[arg(long, requires = "rt_end")]
        rt_start: Option<f64>,
        #[arg(long, requires = "rt_start")]
        rt_end: Option<f64>,
    },
    /// Zoom a chromatogram and click accepted feature markers to inspect MS2 spectra
    Interactive {
        input: PathBuf,
        audit: PathBuf,
        #[arg(long, value_enum, default_value_t = ChromatogramKind::Tic)]
        kind: ChromatogramKind,
        #[arg(long)]
        mz: Option<f64>,
        #[arg(long, default_value_t = 10.0)]
        ppm: f64,
        #[arg(long, default_value_t = 0.003)]
        absolute_mz: f64,
    },
    /// Plot the centroid peaks of a raw MS1 or MS2 scan
    Spectrum {
        input: PathBuf,
        #[arg(long)]
        scan: String,
    },
    /// Plot accepted features in a feature audit TSV
    Features {
        audit: PathBuf,
        #[arg(long, value_enum, default_value_t = FeaturePlotKind::Map)]
        plot: FeaturePlotKind,
    },
    /// Inspect one accepted feature's XIC and its selected MS2 spectrum
    Feature {
        input: PathBuf,
        audit: PathBuf,
        feature_id: usize,
        #[arg(long, default_value_t = 10.0)]
        ppm: f64,
        #[arg(long, default_value_t = 0.003)]
        absolute_mz: f64,
    },
}

fn chromatogram_mode(
    kind: ChromatogramKind,
    mz: Option<f64>,
    ppm: f64,
    absolute_mz: f64,
) -> Result<inspect::Chromatogram> {
    Ok(match kind {
        ChromatogramKind::Tic => {
            ensure!(mz.is_none(), "--mz requires --kind xic");
            inspect::Chromatogram::Tic
        }
        ChromatogramKind::Bpi => {
            ensure!(mz.is_none(), "--mz requires --kind xic");
            inspect::Chromatogram::Bpi
        }
        ChromatogramKind::Xic => inspect::Chromatogram::Xic {
            mz: mz.context("--kind xic requires --mz")?,
            ppm,
            absolute_mz,
        },
    })
}

fn plot_csv(
    x: &str,
    y: &str,
    line: bool,
    data: impl FnOnce(&mut dyn Write) -> Result<usize>,
) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new().context("creating temporary plot data")?;
    data(&mut temp)?;
    temp.flush()?;
    let mut plot = Command::new("xan");
    plot.arg("plot");
    if line {
        plot.arg("--line");
    }
    let status = plot
        .args([x, y])
        .arg(temp.path())
        .status()
        .context("starting xan plot; install xan to use picoto view")?;
    ensure!(status.success(), "xan plot failed: {status}");
    Ok(())
}

fn view(command: ViewCommand) -> Result<()> {
    match command {
        ViewCommand::Chromatogram {
            input,
            kind,
            mz,
            ppm,
            absolute_mz,
            rt_start,
            rt_end,
        } => {
            let mode = chromatogram_mode(kind, mz, ppm, absolute_mz)?;
            let rt_range = rt_start.zip(rt_end);
            println!(
                "{kind:?} chromatogram (seconds / intensity): {}",
                input.display()
            );
            std::io::stdout().flush()?;
            plot_csv("rt_seconds", "intensity", true, |out| {
                inspect::chromatogram(&input, mode, rt_range, out)
            })
        }
        ViewCommand::Interactive {
            input,
            audit,
            kind,
            mz,
            ppm,
            absolute_mz,
        } => explore::run(
            &input,
            &audit,
            chromatogram_mode(kind, mz, ppm, absolute_mz)?,
        ),
        ViewCommand::Spectrum { input, scan } => {
            println!("Centroid spectrum {scan}: {}", input.display());
            std::io::stdout().flush()?;
            plot_csv("mz", "intensity", false, |out| {
                inspect::spectrum(&input, &scan, out)
            })
        }
        ViewCommand::Features { audit, plot } => {
            let (mode, y) = match plot {
                FeaturePlotKind::Map => (feature_view::FeaturePlot::Map, "mz"),
                FeaturePlotKind::Intensity => {
                    (feature_view::FeaturePlot::Intensity, "apex_intensity")
                }
            };
            println!("Accepted feature {plot:?}: {}", audit.display());
            std::io::stdout().flush()?;
            plot_csv("rt_seconds", y, false, |out| {
                feature_view::feature_map(&audit, mode, out)
            })
        }
        ViewCommand::Feature {
            input,
            audit,
            feature_id,
            ppm,
            absolute_mz,
        } => {
            let detail = feature_view::detail(&audit, feature_id)?;
            println!(
                "Feature {feature_id}: m/z {:.6}, apex {:.3}s, intensity {:.0}, quality {:.1}, MS2 scan {}",
                detail.mz,
                detail.rt_seconds,
                detail.apex_intensity,
                detail.quality_score,
                detail.ms2_scan_id
            );
            std::io::stdout().flush()?;
            plot_csv("rt_seconds", "intensity", true, |out| {
                inspect::chromatogram(
                    &input,
                    inspect::Chromatogram::Xic {
                        mz: detail.mz,
                        ppm,
                        absolute_mz,
                    },
                    Some((
                        (detail.rt_seconds - 30.0).max(0.0),
                        detail.rt_seconds + 30.0,
                    )),
                    out,
                )
            })?;
            plot_csv("mz", "intensity", false, |out| {
                inspect::spectrum(&input, &detail.ms2_scan_id, out)
            })
        }
    }
}

fn is_ms(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mzxml") || ext.eq_ignore_ascii_case("mzml"))
}

fn discover(path: &Path, relative: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let child = entry.path();
        let name = entry.file_name();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            discover(&child, &relative.join(&name), files)?;
        } else if kind.is_file() && is_ms(&child) {
            files.push((child, relative.join(&name)));
        }
    }
    Ok(())
}

fn persist_mgf(
    path: &Path,
    features: &[feature::Feature],
    input: &Path,
    paired: bool,
) -> Result<tempfile::NamedTempFile> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().expect("output parent"))?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        output::write_mgf(&mut writer, features, input, paired)?;
        writer.flush()?;
    }
    Ok(temp)
}

fn process(
    input: &Path,
    relative: &Path,
    output_root: &Path,
    mode: OutputMode,
    settings: &Settings,
    data_points: bool,
) -> Result<usize> {
    let folder = output_root.join(relative.parent().unwrap_or_else(|| Path::new("")));
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let stem = relative
        .file_stem()
        .and_then(|s| s.to_str())
        .context("non-Unicode file stem")?;
    let mut table = tempfile::NamedTempFile::new_in(&folder)?;
    let features = {
        let mut writer = BufWriter::new(table.as_file_mut());
        let features = feature::detect(input, settings, &mut writer)?;
        writer.flush()?;
        features
    };
    let paired = folder.join(format!("{stem}_sirius.mgf"));
    let ms2 = folder.join(format!("{stem}_ms2.mgf"));
    let audit = folder.join(format!("{stem}_features.tsv"));
    let list = folder.join(format!("{stem}_feature_list.tsv"));
    let mut list_temp = tempfile::NamedTempFile::new_in(&folder)?;
    {
        let mut writer = BufWriter::new(list_temp.as_file_mut());
        feature_table::write(&mut writer, &features, data_points, settings.compute_fwhm)?;
        writer.flush()?;
    }
    let paired_temp = if matches!(mode, OutputMode::Paired | OutputMode::Both) {
        Some(persist_mgf(&paired, &features, input, true)?)
    } else {
        None
    };
    let ms2_temp = if matches!(mode, OutputMode::Ms2 | OutputMode::Both) {
        Some(persist_mgf(&ms2, &features, input, false)?)
    } else {
        None
    };
    if let Some(temp) = paired_temp {
        temp.persist(&paired)
            .with_context(|| format!("publishing {}", paired.display()))?;
    }
    if let Some(temp) = ms2_temp {
        temp.persist(&ms2)
            .with_context(|| format!("publishing {}", ms2.display()))?;
    }
    list_temp
        .persist(&list)
        .with_context(|| format!("publishing {}", list.display()))?;
    table
        .persist(&audit)
        .with_context(|| format!("publishing {}", audit.display()))?;
    Ok(features.len())
}

fn run(cli: Cli) -> Result<()> {
    ensure!(
        cli.jobs > 0 && cli.min_scans >= 3 && cli.max_ms2_scans > 0 && cli.min_ms2_peaks > 0,
        "jobs, max-ms2-scans and min-ms2-peaks must be positive; min-scans must be at least 3"
    );
    for (name, value) in [
        ("min-height", cli.min_height),
        ("min-contrast", cli.min_contrast),
        ("ppm", cli.ppm),
        ("absolute-mz", cli.absolute_mz),
        ("max-ms2-rt-offset", cli.max_ms2_rt_offset),
    ] {
        ensure!(
            value.is_finite() && value > 0.0,
            "{name} must be finite and positive"
        );
    }
    let data_points = cli.metrics.contains(&ExtraMetric::DataPoints);
    let settings = Settings {
        min_height: cli.min_height,
        min_scans: cli.min_scans,
        min_contrast: cli.min_contrast,
        ppm: cli.ppm,
        absolute_mz: cli.absolute_mz,
        min_ms2_peaks: cli.min_ms2_peaks,
        max_ms2_rt_offset: cli.max_ms2_rt_offset,
        max_ms2_scans: cli.max_ms2_scans,
        compute_fwhm: cli.metrics.contains(&ExtraMetric::Fwhm),
    };
    let mut files = Vec::new();
    if cli.input.is_dir() {
        discover(&cli.input, Path::new(""), &mut files)?;
    } else if cli.input.is_file() && is_ms(&cli.input) {
        files.push((
            cli.input.clone(),
            PathBuf::from(cli.input.file_name().context("input has no file name")?),
        ));
    } else {
        bail!(
            "input must be a readable mzXML/mzML file or directory: {}",
            cli.input.display()
        );
    }
    ensure!(
        !files.is_empty(),
        "no mzXML/mzML files found in {}",
        cli.input.display()
    );
    let mut destinations = HashSet::new();
    for (_, rel) in &files {
        let destination = rel.with_extension("");
        ensure!(
            destinations.insert(destination),
            "multiple raw files would overwrite one output: {}",
            rel.display()
        );
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cli.jobs)
        .build()?;
    let bar = ProgressBar::new(files.len() as u64);
    bar.set_style(ProgressStyle::with_template(
        "{spinner:.cyan} [{bar:35.cyan/blue}] {pos}/{len} files {elapsed_precise}",
    )?);
    let results = pool.install(|| {
        files
            .par_iter()
            .map(|(path, rel)| {
                let result = process(path, rel, &cli.output, cli.mode, &settings, data_points);
                bar.inc(1);
                (path, result)
            })
            .collect::<Vec<_>>()
    });
    bar.finish_and_clear();
    let mut errors = 0;
    for (path, result) in results {
        match result {
            Ok(count) => println!("{}: {count} features", path.display()),
            Err(error) => {
                eprintln!("{}: {error:#}", path.display());
                errors += 1;
            }
        }
    }
    ensure!(errors == 0, "{errors} file(s) failed");
    Ok(())
}

fn main() {
    let result = if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("view")) {
        view(ViewCli::parse_from(std::env::args_os().skip(1)).command)
    } else {
        run(Cli::parse())
    };
    if let Err(error) = result {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
