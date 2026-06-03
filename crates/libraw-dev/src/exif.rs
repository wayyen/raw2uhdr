//! Minimal EXIF (APP1 payload) serializer.
//!
//! Builds the `Exif\0\0` + little-endian TIFF byte block that libultrahdr's
//! `uhdr_enc_set_exif_data` expects (it writes the APP1 marker and length
//! itself). We emit a curated, widely-understood subset of tags from the
//! camera metadata LibRaw parses — enough for viewers to show camera, lens,
//! and capture settings — rather than copying the DNG's raw-oriented IFDs.

/// Curated capture metadata, gamut/orientation neutral.
#[derive(Debug, Clone, Default)]
pub struct RawMetadata {
    pub make: String,
    pub model: String,
    pub software: String,
    /// Capture time as Unix seconds (UTC), if known.
    pub timestamp: Option<i64>,
    /// Exposure time in seconds.
    pub exposure_time: Option<f32>,
    /// Aperture f-number.
    pub fnumber: Option<f32>,
    /// ISO sensitivity.
    pub iso: Option<f32>,
    /// Focal length in mm.
    pub focal_len: Option<f32>,
}

// EXIF/TIFF tag ids.
const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_SOFTWARE: u16 = 0x0131;
const TAG_DATETIME: u16 = 0x0132;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_EXPOSURE_TIME: u16 = 0x829A;
const TAG_FNUMBER: u16 = 0x829D;
const TAG_ISO: u16 = 0x8827;
const TAG_EXIF_VERSION: u16 = 0x9000;
const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
const TAG_FOCAL_LENGTH: u16 = 0x920A;

// TIFF field types.
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_RATIONAL: u16 = 5;
const TYPE_UNDEFINED: u16 = 7;

/// A value to store in an IFD entry.
enum Value {
    Ascii(Vec<u8>), // includes trailing NUL
    Short(u16),
    Long(u32),
    Rational(u32, u32),
    Undefined(Vec<u8>),
}

impl Value {
    fn ascii(s: &str) -> Value {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        Value::Ascii(v)
    }

    fn type_id(&self) -> u16 {
        match self {
            Value::Ascii(_) => TYPE_ASCII,
            Value::Short(_) => TYPE_SHORT,
            Value::Long(_) => TYPE_LONG,
            Value::Rational(_, _) => TYPE_RATIONAL,
            Value::Undefined(_) => TYPE_UNDEFINED,
        }
    }

    fn count(&self) -> u32 {
        match self {
            Value::Ascii(b) | Value::Undefined(b) => b.len() as u32,
            Value::Short(_) | Value::Long(_) | Value::Rational(_, _) => 1,
        }
    }

    /// Raw bytes for the value (used inline if <= 4 bytes, else in overflow).
    fn bytes(&self) -> Vec<u8> {
        match self {
            Value::Ascii(b) | Value::Undefined(b) => b.clone(),
            Value::Short(v) => v.to_le_bytes().to_vec(),
            Value::Long(v) => v.to_le_bytes().to_vec(),
            Value::Rational(n, d) => {
                let mut v = n.to_le_bytes().to_vec();
                v.extend_from_slice(&d.to_le_bytes());
                v
            }
        }
    }
}

struct Entry {
    tag: u16,
    value: Value,
}

/// Serialize an IFD whose entries are already tag-sorted. `overflow_base` is
/// the absolute TIFF offset where this IFD's overflow bytes will live; those
/// bytes are appended to `overflow`. Returns the IFD bytes (count + entries +
/// next-IFD pointer of 0).
fn write_ifd(entries: &[Entry], overflow_base: usize, overflow: &mut Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in entries {
        out.extend_from_slice(&e.tag.to_le_bytes());
        out.extend_from_slice(&e.value.type_id().to_le_bytes());
        out.extend_from_slice(&e.value.count().to_le_bytes());

        let bytes = e.value.bytes();
        if bytes.len() <= 4 {
            let mut field = [0u8; 4];
            field[..bytes.len()].copy_from_slice(&bytes);
            out.extend_from_slice(&field);
        } else {
            let offset = (overflow_base + overflow.len()) as u32;
            out.extend_from_slice(&offset.to_le_bytes());
            overflow.extend_from_slice(&bytes);
            if overflow.len() % 2 == 1 {
                overflow.push(0); // keep word alignment
            }
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // next IFD = none
    out
}

/// Convert seconds to an EXIF-friendly rational.
fn seconds_to_rational(s: f32) -> (u32, u32) {
    if s > 0.0 && s < 1.0 {
        (1, (1.0 / s).round() as u32)
    } else {
        ((s * 1000.0).round() as u32, 1000)
    }
}

/// Format a Unix timestamp (UTC) as EXIF "YYYY:MM:DD HH:MM:SS".
fn format_datetime(ts: i64) -> String {
    // Howard Hinnant's days-from-civil, inverted.
    let secs = ts.rem_euclid(86_400);
    let days = (ts - secs) / 86_400;
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}:{m:02}:{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// Build the EXIF payload (`Exif\0\0` + TIFF) for the given metadata.
pub fn build_exif(meta: &RawMetadata) -> Vec<u8> {
    let datetime = meta.timestamp.map(format_datetime);

    // IFD0 entries (image-level), tag-ascending.
    let mut ifd0 = Vec::new();
    if !meta.make.is_empty() {
        ifd0.push(Entry { tag: TAG_MAKE, value: Value::ascii(&meta.make) });
    }
    if !meta.model.is_empty() {
        ifd0.push(Entry { tag: TAG_MODEL, value: Value::ascii(&meta.model) });
    }
    // Orientation: LibRaw bakes the camera flip into the pixels, so the stored
    // image is upright.
    ifd0.push(Entry { tag: TAG_ORIENTATION, value: Value::Short(1) });
    let software =
        if meta.software.is_empty() { "dng2uhdr".to_string() } else { meta.software.clone() };
    ifd0.push(Entry { tag: TAG_SOFTWARE, value: Value::ascii(&software) });
    if let Some(dt) = &datetime {
        ifd0.push(Entry { tag: TAG_DATETIME, value: Value::ascii(dt) });
    }

    // Exif sub-IFD entries (capture settings), tag-ascending.
    let mut exif = Vec::new();
    if let Some(t) = meta.exposure_time {
        let (n, d) = seconds_to_rational(t);
        exif.push(Entry { tag: TAG_EXPOSURE_TIME, value: Value::Rational(n, d) });
    }
    if let Some(f) = meta.fnumber {
        exif.push(Entry { tag: TAG_FNUMBER, value: Value::Rational((f * 100.0).round() as u32, 100) });
    }
    if let Some(iso) = meta.iso {
        exif.push(Entry { tag: TAG_ISO, value: Value::Short(iso.round().clamp(0.0, 65535.0) as u16) });
    }
    exif.push(Entry { tag: TAG_EXIF_VERSION, value: Value::Undefined(b"0232".to_vec()) });
    if let Some(dt) = &datetime {
        exif.push(Entry { tag: TAG_DATETIME_ORIGINAL, value: Value::ascii(dt) });
    }
    if let Some(fl) = meta.focal_len {
        exif.push(Entry { tag: TAG_FOCAL_LENGTH, value: Value::Rational((fl * 100.0).round() as u32, 100) });
    }

    // Layout: header(8) | IFD0 | Exif-IFD | overflow. IFD0 gets an extra entry
    // (the Exif-IFD pointer), so n0 = ifd0.len() + 1.
    const HEADER: usize = 8;
    let ifd0_len = 2 + (ifd0.len() + 1) * 12 + 4;
    let exif_off = HEADER + ifd0_len;
    let exif_len = 2 + exif.len() * 12 + 4;
    let overflow_base = exif_off + exif_len;

    // Insert the Exif-IFD pointer into IFD0 in tag order (0x8769 sorts last).
    ifd0.push(Entry { tag: TAG_EXIF_IFD, value: Value::Long(exif_off as u32) });
    ifd0.sort_by_key(|e| e.tag);
    exif.sort_by_key(|e| e.tag);

    let mut overflow = Vec::new();
    let ifd0_bytes = write_ifd(&ifd0, overflow_base, &mut overflow);
    let exif_bytes = write_ifd(&exif, overflow_base, &mut overflow);

    let mut tiff = Vec::with_capacity(overflow_base + overflow.len());
    tiff.extend_from_slice(b"II"); // little-endian
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&(HEADER as u32).to_le_bytes()); // IFD0 at offset 8
    tiff.extend_from_slice(&ifd0_bytes);
    tiff.extend_from_slice(&exif_bytes);
    tiff.extend_from_slice(&overflow);

    let mut out = Vec::with_capacity(6 + tiff.len());
    out.extend_from_slice(b"Exif\x00\x00");
    out.extend_from_slice(&tiff);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_formats_known_epoch() {
        // 2021-01-21 09:01:00 UTC = 1611219660
        assert_eq!(format_datetime(1_611_219_660), "2021:01:21 09:01:00");
        // Unix epoch
        assert_eq!(format_datetime(0), "1970:01:01 00:00:00");
    }

    #[test]
    fn builds_parseable_exif_header() {
        let meta = RawMetadata {
            make: "Google".into(),
            model: "Pixel".into(),
            software: "HDR+".into(),
            timestamp: Some(1_611_219_660),
            exposure_time: Some(0.005),
            fnumber: Some(1.7),
            iso: Some(112.0),
            focal_len: Some(6.81),
        };
        let blob = build_exif(&meta);
        assert_eq!(&blob[0..6], b"Exif\x00\x00");
        assert_eq!(&blob[6..8], b"II");
        assert_eq!(u16::from_le_bytes([blob[8], blob[9]]), 42);
        // IFD0 offset
        let ifd0_off = u32::from_le_bytes([blob[10], blob[11], blob[12], blob[13]]) as usize;
        assert_eq!(ifd0_off, 8);
        // IFD0 entry count is sane (>= make, model, orientation, software, datetime, exif-ptr).
        let count = u16::from_le_bytes([blob[6 + 8], blob[6 + 9]]);
        assert_eq!(count, 6);
    }
}
