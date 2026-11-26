//! Minimal DNG (TIFF) reader for the camera-profile and opcode tags LibRaw
//! does not interpret.
//!
//! LibRaw decodes and demosaics DNGs, but (without the Adobe DNG SDK) ignores
//! most of what makes a DNG render the way its maker intended: ForwardMatrix
//! interpolation, the profile look/hue-sat tables, the profile tone curve, the
//! local-tone-mapping gain table map, and OpcodeList2 lens-shading gain maps.
//! This module reads exactly those tags; [`crate::profile`] applies them.
//!
//! Only the structural subset of TIFF needed here is parsed (IFD0 + SubIFDs);
//! anything malformed is skipped rather than treated as fatal, so a damaged
//! profile degrades to the generic LibRaw colour path instead of failing.

use std::path::Path;

/// Row-major 3x3 matrix.
pub type Mat3 = [[f64; 3]; 3];

/// One calibration illuminant's colour data (DNG has up to three).
#[derive(Debug, Clone, Default)]
pub struct Calibration {
    /// EXIF LightSource code (`CalibrationIlluminantN`).
    pub illuminant: u16,
    /// Custom illuminant chromaticity (`IlluminantDataN`), for illuminant 255.
    pub illuminant_xy: Option<[f64; 2]>,
    pub color_matrix: Option<Mat3>,
    pub forward_matrix: Option<Mat3>,
    pub camera_calibration: Option<Mat3>,
    pub hue_sat_map: Option<Vec<[f32; 3]>>,
}

/// A 3D hue/saturation/value table (`ProfileHueSatMap*` or `ProfileLookTable*`).
#[derive(Debug, Clone)]
pub struct HsvTable {
    pub hue_divs: usize,
    pub sat_divs: usize,
    pub val_divs: usize,
    /// `[hue_shift_degrees, sat_scale, val_scale]`, indexed
    /// `[val][hue][sat]` as stored in the DNG.
    pub data: Vec<[f32; 3]>,
    /// Value axis is sRGB-gamma encoded (`*Encoding` = 1) rather than linear.
    pub srgb_encoded: bool,
}

/// `ProfileGainTableMap` (DNG 1.6): a spatially varying, brightness-indexed
/// gain table — in practice the camera's local tone mapping.
#[derive(Debug, Clone)]
pub struct GainTableMap {
    pub points_v: usize,
    pub points_h: usize,
    pub spacing_v: f64,
    pub spacing_h: f64,
    pub origin_v: f64,
    pub origin_h: f64,
    pub points_n: usize,
    /// Input weights for R, G, B, min(RGB), max(RGB).
    pub weights: [f32; 5],
    /// `gains[(v * points_h + h) * points_n + n]`.
    pub gains: Vec<f32>,
}

/// An OpcodeList2 `GainMap` opcode (lens / colour shading on the CFA data).
#[derive(Debug, Clone)]
pub struct GainMapOpcode {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    pub plane: u32,
    pub planes: u32,
    pub row_pitch: u32,
    pub col_pitch: u32,
    pub points_v: usize,
    pub points_h: usize,
    pub spacing_v: f64,
    pub spacing_h: f64,
    pub origin_v: f64,
    pub origin_h: f64,
    pub map_planes: usize,
    pub gains: Vec<f32>,
}

impl GainMapOpcode {
    /// Bilinearly interpolated gain at a position relative to the image area
    /// (`v`, `h` in 0..1), for map plane 0.
    pub fn gain_at(&self, v: f64, h: f64) -> f32 {
        let fv = ((v - self.origin_v) / self.spacing_v).clamp(0.0, (self.points_v - 1) as f64);
        let fh = ((h - self.origin_h) / self.spacing_h).clamp(0.0, (self.points_h - 1) as f64);
        let (v0, h0) = (fv.floor() as usize, fh.floor() as usize);
        let (v1, h1) = ((v0 + 1).min(self.points_v - 1), (h0 + 1).min(self.points_h - 1));
        let (tv, th) = ((fv - v0 as f64) as f32, (fh - h0 as f64) as f32);
        let g = |v: usize, h: usize| self.gains[(v * self.points_h + h) * self.map_planes];
        let top = g(v0, h0) + (g(v0, h1) - g(v0, h0)) * th;
        let bot = g(v1, h0) + (g(v1, h1) - g(v1, h0)) * th;
        top + (bot - top) * tv
    }
}

/// The DNG colour-rendering data relevant to development.
#[derive(Debug, Clone, Default)]
pub struct DngProfile {
    pub calibrations: Vec<Calibration>,
    pub analog_balance: Option<[f64; 3]>,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub as_shot_white_xy: Option<[f64; 2]>,
    pub baseline_exposure: f64,
    pub baseline_exposure_offset: f64,
    /// `DefaultBlackRender`: 0 = auto (apply default shadows), 1 = none.
    pub default_black_render: u32,
    /// `ShadowScale`: scales the shadows (black clip) control.
    pub shadow_scale: f64,
    pub hue_sat_map: Option<HsvTable>,
    pub look_table: Option<HsvTable>,
    /// `(input, output)` pairs on [0, 1].
    pub tone_curve: Option<Vec<(f32, f32)>>,
    pub gain_table_map: Option<GainTableMap>,
    pub opcode_gain_maps: Vec<GainMapOpcode>,
}

impl DngProfile {
    /// True when the profile can drive colour conversion on its own (a
    /// three-channel camera with at least one ColorMatrix).
    pub fn has_color(&self) -> bool {
        self.calibrations.iter().any(|c| c.color_matrix.is_some())
    }
}

// TIFF/DNG tag numbers.
const NEW_SUBFILE_TYPE: u16 = 0x00FE;
const SUB_IFDS: u16 = 0x014A;
const COLOR_MATRIX: [u16; 3] = [0xC621, 0xC622, 0xCD33];
const CAMERA_CALIBRATION: [u16; 3] = [0xC623, 0xC624, 0xCD32];
const FORWARD_MATRIX: [u16; 3] = [0xC714, 0xC715, 0xCD34];
const CALIBRATION_ILLUMINANT: [u16; 3] = [0xC65A, 0xC65B, 0xCD31];
const ILLUMINANT_DATA: [u16; 3] = [0xCD35, 0xCD36, 0xCD37];
const HUE_SAT_MAP_DATA: [u16; 3] = [0xC6FA, 0xC6FB, 0xCD38];
const ANALOG_BALANCE: u16 = 0xC627;
const AS_SHOT_NEUTRAL: u16 = 0xC628;
const AS_SHOT_WHITE_XY: u16 = 0xC629;
const BASELINE_EXPOSURE: u16 = 0xC62A;
const BASELINE_EXPOSURE_OFFSET: u16 = 0xC7A5;
const DEFAULT_BLACK_RENDER: u16 = 0xC7A6;
const SHADOW_SCALE: u16 = 0xC633;
const HUE_SAT_MAP_DIMS: u16 = 0xC6F9;
const HUE_SAT_MAP_ENCODING: u16 = 0xC7A3;
const LOOK_TABLE_DIMS: u16 = 0xC725;
const LOOK_TABLE_DATA: u16 = 0xC726;
const LOOK_TABLE_ENCODING: u16 = 0xC7A4;
const PROFILE_TONE_CURVE: u16 = 0xC6FC;
const PROFILE_GAIN_TABLE_MAP: u16 = 0xCD2D;
const OPCODE_LIST2: u16 = 0xC741;

/// Read the DNG profile from `path`. Returns `None` for non-DNG/non-TIFF
/// files or when the file cannot be read.
pub fn read_profile(path: &Path) -> Option<DngProfile> {
    let data = std::fs::read(path).ok()?;
    parse_profile(&data)
}

fn parse_profile(data: &[u8]) -> Option<DngProfile> {
    let tiff = Tiff::new(data)?;
    let ifd0 = tiff.ifd(tiff.u32(4)? as usize)?;
    // DNGVersion must be present in IFD0 for this to be a DNG.
    ifd0.iter().find(|e| e.tag == 0xC612)?;

    // The raw IFD (NewSubFileType 0) is either IFD0 or one of its SubIFDs;
    // per-raw tags (opcodes, gain table map) may live there.
    let mut raw_ifd = None;
    if tiff.entry_u32(&ifd0, NEW_SUBFILE_TYPE) == Some(0) {
        raw_ifd = Some(ifd0.clone());
    }
    if let Some(e) = ifd0.iter().find(|e| e.tag == SUB_IFDS) {
        for off in tiff.values_u32(e) {
            if let Some(sub) = tiff.ifd(off as usize) {
                if tiff.entry_u32(&sub, NEW_SUBFILE_TYPE) == Some(0) && raw_ifd.is_none() {
                    raw_ifd = Some(sub);
                }
            }
        }
    }
    let raw_ifd = raw_ifd.unwrap_or_else(|| ifd0.clone());
    // Profile tags normally sit in IFD0; fall back to the raw IFD.
    let find = |tag: u16| {
        ifd0.iter().find(|e| e.tag == tag).or_else(|| raw_ifd.iter().find(|e| e.tag == tag))
    };

    let mut p = DngProfile::default();
    for i in 0..3 {
        let cal = Calibration {
            illuminant: find(CALIBRATION_ILLUMINANT[i])
                .and_then(|e| tiff.values_f64(e).first().copied())
                .map(|v| v as u16)
                .unwrap_or(0),
            illuminant_xy: find(ILLUMINANT_DATA[i]).and_then(|e| tiff.illuminant_xy(e)),
            color_matrix: find(COLOR_MATRIX[i]).and_then(|e| mat3(&tiff.values_f64(e))),
            forward_matrix: find(FORWARD_MATRIX[i]).and_then(|e| mat3(&tiff.values_f64(e))),
            camera_calibration: find(CAMERA_CALIBRATION[i])
                .and_then(|e| mat3(&tiff.values_f64(e))),
            hue_sat_map: find(HUE_SAT_MAP_DATA[i]).map(|e| triples(&tiff.values_f64(e))),
        };
        if cal.color_matrix.is_some() {
            p.calibrations.push(cal);
        }
    }
    p.analog_balance = find(ANALOG_BALANCE).and_then(|e| vec3(&tiff.values_f64(e)));
    p.as_shot_neutral = find(AS_SHOT_NEUTRAL).and_then(|e| vec3(&tiff.values_f64(e)));
    p.as_shot_white_xy = find(AS_SHOT_WHITE_XY).and_then(|e| {
        let v = tiff.values_f64(e);
        (v.len() == 2).then(|| [v[0], v[1]])
    });
    p.baseline_exposure =
        find(BASELINE_EXPOSURE).and_then(|e| tiff.values_f64(e).first().copied()).unwrap_or(0.0);
    p.baseline_exposure_offset = find(BASELINE_EXPOSURE_OFFSET)
        .and_then(|e| tiff.values_f64(e).first().copied())
        .unwrap_or(0.0);
    p.default_black_render = find(DEFAULT_BLACK_RENDER)
        .and_then(|e| tiff.values_f64(e).first().copied())
        .unwrap_or(0.0) as u32;
    p.shadow_scale =
        find(SHADOW_SCALE).and_then(|e| tiff.values_f64(e).first().copied()).unwrap_or(1.0);

    let encoding = |tag| find(tag).and_then(|e| tiff.values_f64(e).first().copied()) == Some(1.0);
    if let Some(dims) = find(HUE_SAT_MAP_DIMS).map(|e| tiff.values_f64(e)) {
        // HueSatMaps are per-illuminant; the profile module interpolates them.
        if let Some(first) = p.calibrations.iter().find_map(|c| c.hue_sat_map.clone()) {
            p.hue_sat_map = hsv_table(&dims, first, encoding(HUE_SAT_MAP_ENCODING));
        }
    }
    if let (Some(dims), Some(data)) = (find(LOOK_TABLE_DIMS), find(LOOK_TABLE_DATA)) {
        p.look_table = hsv_table(
            &tiff.values_f64(dims),
            triples(&tiff.values_f64(data)),
            encoding(LOOK_TABLE_ENCODING),
        );
    }
    p.tone_curve = find(PROFILE_TONE_CURVE).and_then(|e| {
        let v = tiff.values_f64(e);
        (v.len() >= 4 && v.len() % 2 == 0)
            .then(|| v.chunks_exact(2).map(|c| (c[0] as f32, c[1] as f32)).collect())
    });
    p.gain_table_map = raw_ifd
        .iter()
        .chain(ifd0.iter())
        .find(|e| e.tag == PROFILE_GAIN_TABLE_MAP)
        .and_then(|e| parse_gain_table_map(tiff.bytes(e)?, tiff.le));
    p.opcode_gain_maps = raw_ifd
        .iter()
        .find(|e| e.tag == OPCODE_LIST2)
        .and_then(|e| tiff.bytes(e))
        .map(parse_gain_map_opcodes)
        .unwrap_or_default();
    Some(p)
}

fn mat3(v: &[f64]) -> Option<Mat3> {
    (v.len() == 9).then(|| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
}

fn vec3(v: &[f64]) -> Option<[f64; 3]> {
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

fn triples(v: &[f64]) -> Vec<[f32; 3]> {
    v.chunks_exact(3).map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect()
}

fn hsv_table(dims: &[f64], data: Vec<[f32; 3]>, srgb_encoded: bool) -> Option<HsvTable> {
    let (hue_divs, sat_divs) = (*dims.first()? as usize, *dims.get(1)? as usize);
    let val_divs = dims.get(2).map(|v| *v as usize).unwrap_or(1).max(1);
    (hue_divs >= 1 && sat_divs >= 2 && data.len() == hue_divs * sat_divs * val_divs).then_some(
        HsvTable { hue_divs, sat_divs, val_divs, data, srgb_encoded },
    )
}

/// ProfileGainTableMap: stored in the file's byte order.
fn parse_gain_table_map(b: &[u8], le: bool) -> Option<GainTableMap> {
    let r = Reader { b, le };
    let points_v = r.u32(0)? as usize;
    let points_h = r.u32(4)? as usize;
    let (spacing_v, spacing_h) = (r.f64(8)?, r.f64(16)?);
    let (origin_v, origin_h) = (r.f64(24)?, r.f64(32)?);
    let points_n = r.u32(40)? as usize;
    let mut weights = [0f32; 5];
    for (i, w) in weights.iter_mut().enumerate() {
        *w = r.f32(44 + 4 * i)?;
    }
    let count = points_v.checked_mul(points_h)?.checked_mul(points_n)?;
    if points_v == 0 || points_h == 0 || points_n < 2 || b.len() < 64 + count * 4 {
        return None;
    }
    let gains = (0..count).map(|i| r.f32(64 + 4 * i)).collect::<Option<Vec<_>>>()?;
    Some(GainTableMap {
        points_v,
        points_h,
        spacing_v,
        spacing_h,
        origin_v,
        origin_h,
        points_n,
        weights,
        gains,
    })
}

/// Extract the `GainMap` (id 9) opcodes from an OpcodeList (always big-endian).
fn parse_gain_map_opcodes(b: &[u8]) -> Vec<GainMapOpcode> {
    let r = Reader { b, le: false };
    let mut out = Vec::new();
    let Some(count) = r.u32(0) else { return out };
    let mut p = 4;
    for _ in 0..count {
        let (Some(id), Some(size)) = (r.u32(p), r.u32(p + 12)) else { break };
        let q = p + 16;
        if id == 9 {
            if let Some(op) = parse_gain_map(&r, q) {
                out.push(op);
            }
        }
        p = q + size as usize;
    }
    out
}

fn parse_gain_map(r: &Reader<'_>, q: usize) -> Option<GainMapOpcode> {
    let u = |i: usize| r.u32(q + 4 * i);
    let points_v = u(8)? as usize;
    let points_h = u(9)? as usize;
    let map_planes = r.u32(q + 72)? as usize;
    let count = points_v.checked_mul(points_h)?.checked_mul(map_planes)?;
    if points_v == 0 || points_h == 0 || map_planes == 0 {
        return None;
    }
    let gains = (0..count).map(|i| r.f32(q + 76 + 4 * i)).collect::<Option<Vec<_>>>()?;
    Some(GainMapOpcode {
        top: u(0)?,
        left: u(1)?,
        bottom: u(2)?,
        right: u(3)?,
        plane: u(4)?,
        planes: u(5)?,
        row_pitch: u(6)?.max(1),
        col_pitch: u(7)?.max(1),
        points_v,
        points_h,
        spacing_v: r.f64(q + 40)?,
        spacing_h: r.f64(q + 48)?,
        origin_v: r.f64(q + 56)?,
        origin_h: r.f64(q + 64)?,
        map_planes,
        gains,
    })
}

/// Bounds-checked scalar reads with a fixed byte order.
struct Reader<'a> {
    b: &'a [u8],
    le: bool,
}

impl Reader<'_> {
    fn arr<const N: usize>(&self, off: usize) -> Option<[u8; N]> {
        let mut a: [u8; N] = self.b.get(off..off.checked_add(N)?)?.try_into().ok()?;
        if !self.le {
            a.reverse();
        }
        Some(a)
    }
    fn u16(&self, off: usize) -> Option<u16> {
        self.arr(off).map(u16::from_le_bytes)
    }
    fn u32(&self, off: usize) -> Option<u32> {
        self.arr(off).map(u32::from_le_bytes)
    }
    fn f32(&self, off: usize) -> Option<f32> {
        self.arr(off).map(f32::from_le_bytes)
    }
    fn f64(&self, off: usize) -> Option<f64> {
        self.arr(off).map(f64::from_le_bytes)
    }
}

#[derive(Debug, Clone)]
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    /// Offset of the 4-byte value/offset field within the file.
    field: usize,
}

struct Tiff<'a> {
    r: Reader<'a>,
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(b: &'a [u8]) -> Option<Self> {
        let le = match b.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let t = Tiff { r: Reader { b, le }, le };
        (t.r.u16(2)? == 42).then_some(t)
    }

    fn u32(&self, off: usize) -> Option<u32> {
        self.r.u32(off)
    }

    fn ifd(&self, off: usize) -> Option<Vec<Entry>> {
        let n = self.r.u16(off)? as usize;
        (0..n)
            .map(|i| {
                let e = off + 2 + 12 * i;
                Some(Entry {
                    tag: self.r.u16(e)?,
                    typ: self.r.u16(e + 2)?,
                    count: self.r.u32(e + 4)?,
                    field: e + 8,
                })
            })
            .collect()
    }

    fn type_size(typ: u16) -> usize {
        match typ {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 => 8,
            _ => 0,
        }
    }

    /// Offset of an entry's value data (inline when it fits in 4 bytes).
    fn data_offset(&self, e: &Entry) -> Option<(usize, usize)> {
        let len = Self::type_size(e.typ).checked_mul(e.count as usize)?;
        let off = if len <= 4 { e.field } else { self.r.u32(e.field)? as usize };
        (off.checked_add(len)? <= self.r.b.len()).then_some((off, len))
    }

    fn bytes(&self, e: &Entry) -> Option<&'a [u8]> {
        let (off, len) = self.data_offset(e)?;
        self.r.b.get(off..off + len)
    }

    fn values_f64(&self, e: &Entry) -> Vec<f64> {
        let Some((off, _)) = self.data_offset(e) else { return Vec::new() };
        let sz = Self::type_size(e.typ);
        let r = &self.r;
        (0..e.count as usize)
            .map_while(|i| {
                let o = off + i * sz;
                Some(match e.typ {
                    1 | 7 => *r.b.get(o)? as f64,
                    6 => *r.b.get(o)? as i8 as f64,
                    3 => r.u16(o)? as f64,
                    8 => r.u16(o)? as i16 as f64,
                    4 | 13 => r.u32(o)? as f64,
                    9 => r.u32(o)? as i32 as f64,
                    5 => r.u32(o)? as f64 / (r.u32(o + 4)?.max(1)) as f64,
                    10 => r.u32(o)? as i32 as f64 / (r.u32(o + 4)? as i32).max(1) as f64,
                    11 => r.f32(o)? as f64,
                    12 => r.f64(o)?,
                    _ => return None,
                })
            })
            .collect()
    }

    fn values_u32(&self, e: &Entry) -> Vec<u32> {
        self.values_f64(e).into_iter().map(|v| v as u32).collect()
    }

    fn entry_u32(&self, ifd: &[Entry], tag: u16) -> Option<u32> {
        ifd.iter().find(|e| e.tag == tag).and_then(|e| self.values_u32(e).first().copied())
    }

    /// `IlluminantDataN`: type 0 = xy chromaticity as two URationals.
    fn illuminant_xy(&self, e: &Entry) -> Option<[f64; 2]> {
        let b = self.bytes(e)?;
        let r = Reader { b, le: self.le };
        if r.u16(0)? != 0 {
            return None;
        }
        let rat = |o: usize| Some(r.u32(o)? as f64 / r.u32(o + 4)?.max(1) as f64);
        Some([rat(2)?, rat(10)?])
    }
}
