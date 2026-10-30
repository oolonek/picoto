use std::io::Write;

use anyhow::Result;

use crate::feature::Feature;

pub fn write(
    out: &mut dyn Write,
    features: &[Feature],
    data_points: bool,
    fwhm: bool,
) -> Result<()> {
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .terminator(csv::Terminator::Any(b'\n'))
        .has_headers(false)
        .from_writer(out);

    match (data_points, fwhm) {
        (false, false) => writer.write_record([
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
        ])?,
        (true, false) => writer.write_record([
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
            "ms1_data_points",
        ])?,
        (false, true) => writer.write_record([
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
            "fwhm_seconds",
        ])?,
        (true, true) => writer.write_record([
            "feature_id",
            "mz",
            "rt_seconds",
            "peak_height",
            "peak_area",
            "isotope_peaks",
            "quality_score",
            "ms1_data_points",
            "fwhm_seconds",
        ])?,
    }

    for feature in features {
        match (data_points, fwhm) {
            (false, false) => writer.serialize((
                feature.id,
                feature.mz,
                feature.rt_seconds,
                feature.peak_height,
                feature.peak_area,
                feature.isotope_peaks,
                feature.quality_score,
            ))?,
            (true, false) => writer.serialize((
                feature.id,
                feature.mz,
                feature.rt_seconds,
                feature.peak_height,
                feature.peak_area,
                feature.isotope_peaks,
                feature.quality_score,
                feature.ms1_data_points,
            ))?,
            (false, true) => writer.serialize((
                feature.id,
                feature.mz,
                feature.rt_seconds,
                feature.peak_height,
                feature.peak_area,
                feature.isotope_peaks,
                feature.quality_score,
                feature.fwhm_seconds,
            ))?,
            (true, true) => writer.serialize((
                feature.id,
                feature.mz,
                feature.rt_seconds,
                feature.peak_height,
                feature.peak_area,
                feature.isotope_peaks,
                feature.quality_score,
                feature.ms1_data_points,
                feature.fwhm_seconds,
            ))?,
        }
    }
    writer.flush()?;
    Ok(())
}
