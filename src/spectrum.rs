use std::path::Path;

use anyhow::Result;

#[derive(Clone, Copy, Debug)]
pub struct Peak {
    pub mz: f64,
    pub intensity: f64,
}

#[derive(Clone, Debug)]
pub struct Precursor {
    pub mz: f64,
    pub charge: Option<i32>,
    pub intensity: Option<f64>,
    pub collision_energy: Option<f64>,
    pub isolation_lower: Option<f64>,
    pub isolation_upper: Option<f64>,
}

#[derive(Debug)]
pub struct Spectrum {
    pub id: String,
    pub ms_level: u8,
    pub rt_seconds: f64,
    pub polarity: i8,
    pub centroided: bool,
    pub peaks: Vec<Peak>,
    pub precursor: Option<Precursor>,
}

pub type Sink<'a> = dyn FnMut(Spectrum) -> Result<()> + 'a;

pub fn read_spectra(path: &Path, sink: &mut Sink<'_>) -> Result<()> {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mzxml") => crate::mzxml::read(path, sink),
        Some("mzml") => crate::mzml::read(path, sink),
        _ => anyhow::bail!("unsupported mass spectrometry file: {}", path.display()),
    }
}
