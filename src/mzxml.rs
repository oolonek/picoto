use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::read::ZlibDecoder;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::spectrum::{Peak, Precursor, Sink, Spectrum};

struct Scan {
    spectrum: Spectrum,
    declared_peaks: Option<usize>,
    saw_peaks: bool,
    collision_energy: Option<f64>,
}

struct Encoding {
    width: usize,
    little_endian: bool,
    compressed: bool,
    compressed_len: Option<usize>,
}

fn attribute(
    tag: &BytesStart<'_>,
    key: &[u8],
    decoder: quick_xml::encoding::Decoder,
) -> Result<Option<String>> {
    for item in tag.attributes() {
        let item = item.context("invalid XML attribute")?;
        if item.key.as_ref() == key {
            return Ok(Some(item.decode_and_unescape_value(decoder)?.into_owned()));
        }
    }
    Ok(None)
}

fn required(
    tag: &BytesStart<'_>,
    key: &[u8],
    decoder: quick_xml::encoding::Decoder,
) -> Result<String> {
    attribute(tag, key, decoder)?
        .with_context(|| format!("missing {} attribute", String::from_utf8_lossy(key)))
}

fn number<T: std::str::FromStr>(text: &str, field: &str) -> Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    text.parse()
        .with_context(|| format!("invalid {field}: {text:?}"))
}

fn optional_number<T: std::str::FromStr>(
    tag: &BytesStart<'_>,
    key: &[u8],
    decoder: quick_xml::encoding::Decoder,
) -> Result<Option<T>>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    attribute(tag, key, decoder)?
        .map(|value| number(&value, &String::from_utf8_lossy(key)))
        .transpose()
}

fn duration_seconds(text: &str) -> Result<f64> {
    let rest = text
        .strip_prefix('P')
        .context("retentionTime must be an ISO 8601 duration")?;
    let mut time = false;
    let mut total = 0.0;
    let mut offset = 0;
    let mut any = false;
    let mut last_rank = 0;
    let mut previous_fraction = false;
    for (i, ch) in rest.char_indices() {
        if ch == 'T' {
            ensure!(!time && i == offset, "invalid retentionTime: {text:?}");
            time = true;
            offset = i + 1;
            continue;
        }
        let (rank, factor) = match (time, ch) {
            (false, 'W') => (1, 604800.0),
            (false, 'D') => (2, 86400.0),
            (true, 'H') => (3, 3600.0),
            (true, 'M') => (4, 60.0),
            (true, 'S') => (5, 1.0),
            _ => continue,
        };
        ensure!(
            rank > last_rank && !previous_fraction && i > offset,
            "invalid retentionTime: {text:?}"
        );
        let part = &rest[offset..i];
        ensure!(
            part.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && part.bytes().filter(|&b| b == b'.').count() <= 1,
            "invalid retentionTime: {text:?}"
        );
        let amount: f64 = number(part, "retentionTime")?;
        ensure!(amount.is_finite(), "non-finite retentionTime: {text:?}");
        total += amount * factor;
        previous_fraction = part.contains('.');
        any = true;
        last_rank = rank;
        offset = i + 1;
    }
    ensure!(
        any && offset == rest.len() && total.is_finite(),
        "invalid retentionTime: {text:?}"
    );
    Ok(total)
}

fn start_scan(tag: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Result<Scan> {
    let id = required(tag, b"num", decoder)?;
    ensure!(!id.is_empty(), "empty scan number");
    let ms_level: u8 = number(&required(tag, b"msLevel", decoder)?, "msLevel")?;
    ensure!(ms_level > 0, "scan {id}: msLevel must be positive");
    let rt_seconds = duration_seconds(&required(tag, b"retentionTime", decoder)?)
        .with_context(|| format!("scan {id}"))?;
    let centroided = match attribute(tag, b"centroided", decoder)?.as_deref() {
        Some("1" | "true") => true,
        Some("0" | "false") => bail!("scan {id}: profile-mode scan is unsupported (centroided=0)"),
        other => bail!("scan {id}: unsupported or missing centroided attribute: {other:?}"),
    };
    let polarity = match attribute(tag, b"polarity", decoder)?.as_deref() {
        Some("+") => 1,
        Some("-") => -1,
        None | Some("") => 0,
        other => bail!("scan {id}: invalid polarity: {other:?}"),
    };
    let declared_peaks = optional_number(tag, b"peaksCount", decoder)?;
    let collision_energy: Option<f64> = optional_number(tag, b"collisionEnergy", decoder)?;
    if let Some(value) = collision_energy {
        ensure!(value.is_finite(), "scan {id}: non-finite collisionEnergy");
    }
    Ok(Scan {
        spectrum: Spectrum {
            id,
            ms_level,
            rt_seconds,
            polarity,
            centroided,
            peaks: Vec::new(),
            precursor: None,
        },
        declared_peaks,
        saw_peaks: false,
        collision_energy,
    })
}

fn start_precursor(
    tag: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    scan: &mut Scan,
) -> Result<Precursor> {
    ensure!(
        scan.spectrum.precursor.is_none(),
        "duplicate precursorMz in scan {}",
        scan.spectrum.id
    );
    let charge = optional_number(tag, b"precursorCharge", decoder)?;
    let intensity: Option<f64> = optional_number(tag, b"precursorIntensity", decoder)?;
    let collision_energy: Option<f64> = optional_number(tag, b"collisionEnergy", decoder)?;
    let width: Option<f64> = optional_number(tag, b"windowWideness", decoder)?;
    for (name, value) in [
        ("precursorIntensity", intensity),
        ("collisionEnergy", collision_energy),
        ("windowWideness", width),
    ] {
        if let Some(value) = value {
            ensure!(
                value.is_finite() && (name != "windowWideness" || value >= 0.0),
                "scan {}: invalid {name}: {value}",
                scan.spectrum.id
            );
        }
    }
    Ok(Precursor {
        mz: 0.0,
        charge,
        intensity,
        collision_energy: collision_energy.or(scan.collision_energy),
        isolation_lower: width.map(|w| w / 2.0),
        isolation_upper: width.map(|w| w / 2.0),
    })
}

fn peaks_encoding(tag: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Result<Encoding> {
    let width = match required(tag, b"precision", decoder)?.as_str() {
        "32" => 4,
        "64" => 8,
        other => bail!("unsupported mzXML peak precision: {other}"),
    };
    let little_endian = match attribute(tag, b"byteOrder", decoder)?.as_deref() {
        None | Some("network" | "big") => false,
        Some("little") => true,
        other => bail!("unsupported mzXML peak byteOrder: {other:?}"),
    };
    let compressed = match attribute(tag, b"compressionType", decoder)?.as_deref() {
        None | Some("none" | "no compression") => false,
        Some("zlib") => true,
        other => bail!("unsupported mzXML peak compression: {other:?}"),
    };
    if let Some(content_type) = attribute(tag, b"contentType", decoder)? {
        ensure!(
            content_type == "m/z-int",
            "unsupported mzXML peak contentType: {content_type}"
        );
    }
    Ok(Encoding {
        width,
        little_endian,
        compressed,
        compressed_len: optional_number(tag, b"compressedLen", decoder)?,
    })
}

fn decode_peaks(text: &[u8], encoding: &Encoding, count: Option<usize>) -> Result<Vec<Peak>> {
    let encoded = if text.iter().any(u8::is_ascii_whitespace) {
        let clean: Vec<u8> = text
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        STANDARD.decode(clean)
    } else {
        STANDARD.decode(text)
    }
    .context("invalid base64 peaks payload")?;
    if let Some(len) = encoding.compressed_len {
        ensure!(
            len == encoded.len(),
            "compressedLen {len} does not match {} payload bytes",
            encoded.len()
        );
    }
    let data = if encoding.compressed {
        let mut data = Vec::new();
        ZlibDecoder::new(encoded.as_slice())
            .read_to_end(&mut data)
            .context("invalid zlib peaks payload")?;
        data
    } else {
        encoded
    };
    let pair_width = 2 * encoding.width;
    ensure!(
        data.len() % pair_width == 0,
        "peak payload length {} is not a multiple of {pair_width}",
        data.len()
    );
    if let Some(count) = count {
        ensure!(
            data.len() / pair_width == count,
            "peaksCount {count} does not match decoded {} peaks",
            data.len() / pair_width
        );
    }
    let mut peaks = Vec::with_capacity(data.len() / pair_width);
    for pair in data.chunks_exact(pair_width) {
        let value = |bytes: &[u8]| -> f64 {
            match (encoding.width, encoding.little_endian) {
                (4, true) => f32::from_le_bytes(bytes.try_into().unwrap()) as f64,
                (4, false) => f32::from_be_bytes(bytes.try_into().unwrap()) as f64,
                (8, true) => f64::from_le_bytes(bytes.try_into().unwrap()),
                (8, false) => f64::from_be_bytes(bytes.try_into().unwrap()),
                _ => unreachable!(),
            }
        };
        let mz = value(&pair[..encoding.width]);
        let intensity = value(&pair[encoding.width..]);
        ensure!(
            mz.is_finite() && intensity.is_finite(),
            "non-finite mzXML peak value"
        );
        peaks.push(Peak { mz, intensity });
    }
    Ok(peaks)
}

fn finish_scan(scan: Scan, sink: &mut Sink<'_>) -> Result<()> {
    if !scan.saw_peaks {
        ensure!(
            scan.declared_peaks.unwrap_or(0) == 0,
            "scan {}: missing peaks payload",
            scan.spectrum.id
        );
    }
    sink(scan.spectrum)
}

pub fn read(path: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = Reader::from_reader(BufReader::new(file));
    let mut buf = Vec::new();
    let mut depth = 0usize;
    // Parent scans precede their children in mzXML. Emit a parent before
    // entering its first nested scan, then ignore its eventual closing tag.
    let mut emitted_parents = Vec::new();
    let mut scan: Option<(usize, Scan)> = None;
    let mut peaks: Option<(usize, Encoding, Vec<u8>)> = None;
    let mut precursor: Option<(usize, Precursor, String)> = None;
    let mut saw_root = false;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .with_context(|| format!("reading mzXML at byte {}", reader.buffer_position()))?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                let name = tag.local_name();
                let name = name.as_ref();
                if depth == 0 {
                    ensure!(name == b"mzXML", "expected mzXML root element");
                    ensure!(!saw_root, "multiple mzXML root elements");
                    saw_root = true;
                } else if name == b"scan" {
                    if let Some((scan_depth, parent)) = scan.take() {
                        ensure!(depth == scan_depth, "scan nested inside non-scan element");
                        ensure!(
                            parent.saw_peaks || parent.declared_peaks == Some(0),
                            "scan {}: nested scan before parent peaks",
                            parent.spectrum.id
                        );
                        finish_scan(parent, sink)?;
                        emitted_parents.push(scan_depth);
                    }
                    scan = Some((depth + 1, start_scan(&tag, reader.decoder())?));
                } else if let Some((scan_depth, current)) = scan.as_mut() {
                    if depth == *scan_depth && name == b"peaks" {
                        ensure!(
                            !current.saw_peaks,
                            "scan {}: duplicate peaks element",
                            current.spectrum.id
                        );
                        current.saw_peaks = true;
                        peaks = Some((
                            depth + 1,
                            peaks_encoding(&tag, reader.decoder())?,
                            Vec::new(),
                        ));
                    } else if depth == *scan_depth && name == b"precursorMz" {
                        precursor = Some((
                            depth + 1,
                            start_precursor(&tag, reader.decoder(), current)?,
                            String::new(),
                        ));
                    }
                }
                if scan.is_none()
                    && emitted_parents.last() == Some(&depth)
                    && (name == b"peaks" || name == b"precursorMz")
                {
                    bail!("mzXML parent scan contains peaks or precursor after a nested scan");
                }
                depth += 1;
                if empty {
                    if let Some((_, encoding, data)) = peaks.take_if(|(d, _, _)| *d == depth) {
                        let current = &mut scan.as_mut().unwrap().1;
                        current.spectrum.peaks =
                            decode_peaks(&data, &encoding, current.declared_peaks)
                                .with_context(|| format!("scan {}", current.spectrum.id))?;
                    }
                    ensure!(
                        precursor.as_ref().is_none_or(|(d, _, _)| *d != depth),
                        "empty precursorMz element"
                    );
                    if let Some((_, complete)) = scan.take_if(|(d, _)| *d == depth) {
                        finish_scan(complete, sink)?;
                    }
                    if emitted_parents.last() == Some(&depth) {
                        emitted_parents.pop();
                    }
                    depth -= 1;
                }
            }
            Event::Text(text) => {
                if let Some((d, _, data)) = peaks.as_mut() {
                    if *d == depth {
                        data.extend_from_slice(text.as_ref());
                    }
                }
                if let Some((d, _, value)) = precursor.as_mut() {
                    if *d == depth {
                        value.push_str(&text.decode()?);
                    }
                }
            }
            Event::CData(text) => {
                if let Some((d, _, data)) = peaks.as_mut() {
                    if *d == depth {
                        data.extend_from_slice(text.as_ref());
                    }
                }
                if let Some((d, _, value)) = precursor.as_mut() {
                    if *d == depth {
                        value.push_str(&text.decode()?);
                    }
                }
            }
            Event::End(tag) => {
                ensure!(depth > 0, "unexpected closing XML tag");
                if tag.local_name().as_ref() == b"peaks" {
                    if let Some((_, encoding, data)) = peaks.take_if(|(d, _, _)| *d == depth) {
                        let current = &mut scan.as_mut().unwrap().1;
                        current.spectrum.peaks =
                            decode_peaks(&data, &encoding, current.declared_peaks)
                                .with_context(|| format!("scan {}", current.spectrum.id))?;
                    }
                } else if tag.local_name().as_ref() == b"precursorMz" {
                    if let Some((_, mut info, value)) = precursor.take_if(|(d, _, _)| *d == depth) {
                        let current = &mut scan.as_mut().unwrap().1;
                        info.mz = number(value.trim(), "precursorMz")?;
                        info.isolation_lower = info.isolation_lower.map(|offset| info.mz - offset);
                        info.isolation_upper = info.isolation_upper.map(|offset| info.mz + offset);
                        ensure!(
                            info.mz.is_finite(),
                            "scan {}: non-finite precursorMz",
                            current.spectrum.id
                        );
                        current.spectrum.precursor = Some(info);
                    }
                } else if tag.local_name().as_ref() == b"scan" {
                    if let Some((_, complete)) = scan.take_if(|(d, _)| *d == depth) {
                        finish_scan(complete, sink)?;
                    }
                    if emitted_parents.last() == Some(&depth) {
                        emitted_parents.pop();
                    }
                }
                depth -= 1;
            }
            Event::Eof => {
                ensure!(
                    depth == 0 && saw_root && scan.is_none() && emitted_parents.is_empty(),
                    "truncated or empty mzXML document"
                );
                break;
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}
