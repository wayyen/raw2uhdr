//! DNG camera-profile rendering, following the Adobe DNG SDK reference
//! (`dng_color_spec`, `dng_render`, `RefBaselineHueSatMap`,
//! `RefBaselineProfileGainTableMap`) — restricted to the scene-referred stages:
//! camera -> ProPhoto matrix, HueSatMap, ProfileGainTableMap, exposure ramp and
//! LookTable. The tone curve is display-referred and is applied by the renderer
//! (which also builds the HDR rendition), so it is only carried as data here.
//!
//! Unlike the SDK's SDR path we do not clamp to [0, 1] after each stage: values
//! above 1.0 are real highlight data that the HDR rendition uses.

use crate::color::{self, Mat3};
use crate::dng::{Calibration, DngProfile, GainTableMap, HsvTable};

/// Robertson isotemperature lines: (mired, u, v, slope).
const TEMP_TABLE: [(f64, f64, f64, f64); 31] = [
    (0.0, 0.18006, 0.26352, -0.24341),
    (10.0, 0.18066, 0.26589, -0.25479),
    (20.0, 0.18133, 0.26846, -0.26876),
    (30.0, 0.18208, 0.27119, -0.28539),
    (40.0, 0.18293, 0.27407, -0.30470),
    (50.0, 0.18388, 0.27709, -0.32675),
    (60.0, 0.18494, 0.28021, -0.35156),
    (70.0, 0.18611, 0.28342, -0.37915),
    (80.0, 0.18740, 0.28668, -0.40955),
    (90.0, 0.18880, 0.28997, -0.44278),
    (100.0, 0.19032, 0.29326, -0.47888),
    (125.0, 0.19462, 0.30141, -0.58204),
    (150.0, 0.19962, 0.30921, -0.70471),
    (175.0, 0.20525, 0.31647, -0.84901),
    (200.0, 0.21142, 0.32312, -1.0182),
    (225.0, 0.21807, 0.32909, -1.2168),
    (250.0, 0.22511, 0.33439, -1.4512),
    (275.0, 0.23247, 0.33904, -1.7298),
    (300.0, 0.24010, 0.34308, -2.0637),
    (325.0, 0.24702, 0.34655, -2.4681),
    (350.0, 0.25591, 0.34951, -2.9641),
    (375.0, 0.26400, 0.35200, -3.5814),
    (400.0, 0.27218, 0.35407, -4.3633),
    (425.0, 0.28039, 0.35577, -5.3762),
    (450.0, 0.28863, 0.35714, -6.7262),
    (475.0, 0.29685, 0.35823, -8.5955),
    (500.0, 0.30505, 0.35907, -11.324),
    (525.0, 0.31320, 0.35968, -15.628),
    (550.0, 0.32129, 0.36011, -23.325),
    (575.0, 0.32931, 0.36038, -40.770),
    (600.0, 0.33724, 0.36051, -116.45),
];

/// Temperature (K) and tint of an xy chromaticity (DNG SDK `dng_temperature`).
pub fn xy_to_temp_tint(xy: [f64; 2]) -> (f64, f64) {
    let d = 1.5 - xy[0] + 6.0 * xy[1];
    let (u, v) = (2.0 * xy[0] / d, 3.0 * xy[1] / d);
    let (mut last_dt, mut last_du, mut last_dv) = (0.0, 0.0, 0.0);
    for i in 1..=30 {
        let (r0, u0, v0, _) = TEMP_TABLE[i - 1];
        let (r1, u1, v1, t1) = TEMP_TABLE[i];
        let len = (1.0 + t1 * t1).sqrt();
        let (du, dv) = (1.0 / len, t1 / len);
        let dt = -(u - u1) * dv + (v - v1) * du;
        if dt <= 0.0 || i == 30 {
            let dt = -dt.min(0.0);
            let f = if i == 1 { 0.0 } else { dt / (last_dt + dt) };
            let temp = 1.0e6 / (r0 * f + r1 * (1.0 - f));
            let uu = u - (u0 * f + u1 * (1.0 - f));
            let vv = v - (v0 * f + v1 * (1.0 - f));
            let (du, dv) = (du * (1.0 - f) + last_du * f, dv * (1.0 - f) + last_dv * f);
            let len = (du * du + dv * dv).sqrt();
            return (temp, (uu * du / len + vv * dv / len) * -3000.0);
        }
        (last_dt, last_du, last_dv) = (dt, du, dv);
    }
    (5000.0, 0.0)
}

/// xy chromaticity of a blackbody-ish source at `temp` K with zero tint.
pub fn temp_to_xy(temp: f64) -> [f64; 2] {
    let r = 1.0e6 / temp.max(1.0);
    for i in 0..30 {
        let (r0, u0, v0, _) = TEMP_TABLE[i];
        let (r1, u1, v1, _) = TEMP_TABLE[i + 1];
        if r < r1 || i == 29 {
            let f = ((r1 - r) / (r1 - r0)).clamp(0.0, 1.0);
            let (u, v) = (u0 * f + u1 * (1.0 - f), v0 * f + v1 * (1.0 - f));
            let d = u - 4.0 * v + 2.0;
            return [1.5 * u / d, v / d];
        }
    }
    [0.3457, 0.3585]
}

/// xy of a calibration illuminant (standard D-series exactly, others via CCT).
fn illuminant_xy(cal: &Calibration) -> Option<[f64; 2]> {
    if cal.illuminant == 255 {
        return cal.illuminant_xy;
    }
    Some(match cal.illuminant {
        17 => [0.4476, 0.4074],
        23 => [0.3457, 0.3585],
        20 => [0.3324, 0.3474],
        21 => [0.3127, 0.3290],
        22 => [0.2990, 0.3149],
        code => temp_to_xy(color::illuminant_cct(code)?),
    })
}

/// Per-calibration interpolation weights for a scene white (DNG SDK
/// `dng_color_spec` dual/triple-illuminant logic).
fn calibration_weights(cals: &[Calibration], white: [f64; 2]) -> Vec<f64> {
    let xys: Vec<Option<[f64; 2]>> = cals.iter().map(illuminant_xy).collect();
    match cals.len() {
        0 => vec![],
        1 => vec![1.0],
        2 => {
            let (Some(a), Some(b)) = (xys[0], xys[1]) else { return vec![1.0, 0.0] };
            let (t1, t2) = (xy_to_temp_tint(a).0, xy_to_temp_tint(b).0);
            if (t1 - t2).abs() < 1.0 {
                return vec![1.0, 0.0];
            }
            let t = xy_to_temp_tint(white).0;
            let (lo, hi, swap) = if t1 <= t2 { (t1, t2, false) } else { (t2, t1, true) };
            let g = if t <= lo {
                1.0
            } else if t >= hi {
                0.0
            } else {
                (1.0 / t - 1.0 / hi) / (1.0 / lo - 1.0 / hi)
            };
            if swap {
                vec![1.0 - g, g]
            } else {
                vec![g, 1.0 - g]
            }
        }
        _ => {
            // Triple illuminant (DNG 1.6): inverse-distance weights in a
            // (tint, inverse-temperature) plane, smoothstepped and renormalized.
            let pt = |xy: [f64; 2]| {
                let (t, tint) = xy_to_temp_tint(xy);
                [tint / 200.0, (1500.0 / t).min(1.0)]
            };
            let p = pt(white);
            let mut w: Vec<f64> = xys
                .iter()
                .take(3)
                .map(|xy| {
                    let q = pt(xy.unwrap_or([0.3457, 0.3585]));
                    1.0 / ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + 1e-8)
                })
                .collect();
            let sum: f64 = w.iter().sum();
            for x in &mut w {
                let n = *x / sum;
                *x = (((n * n) * (3.0 - 2.0 * n) - 0.02) / 0.98).clamp(0.0, 1.0);
            }
            let sum: f64 = w.iter().sum::<f64>().max(1e-12);
            let (w1, w2) = (w[0] / sum, w[1] / sum);
            vec![w1, w2, (1.0 - w1 - w2).max(0.0)]
        }
    }
}

fn blend(cals: &[Calibration], w: &[f64], pick: impl Fn(&Calibration) -> Option<Mat3>) -> Option<Mat3> {
    let mut m = [[0.0; 3]; 3];
    let mut total = 0.0;
    for (c, &wi) in cals.iter().zip(w) {
        let Some(x) = pick(c) else {
            if wi > 0.0 {
                return None;
            }
            continue;
        };
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += wi * x[i][j];
            }
        }
        total += wi;
    }
    (total > 0.0).then_some(m)
}

/// The colour transform for one scene white.
pub struct CameraColor {
    /// White-balanced camera value of a neutral, normalized so max = 1
    /// (the DNG SDK's `CameraWhite`).
    pub camera_white: [f64; 3],
    /// Camera RGB -> linear ProPhoto.
    pub camera_to_prophoto: Mat3,
    pub weights: Vec<f64>,
}

impl DngProfile {
    fn analog(&self) -> Mat3 {
        color::diag(self.analog_balance.unwrap_or([1.0; 3]))
    }

    /// XYZ -> camera (AB * CC * CM), interpolated for a white.
    fn xyz_to_camera(&self, w: &[f64]) -> Option<Mat3> {
        let ab = self.analog();
        blend(&self.calibrations, w, |c| {
            let cc = c.camera_calibration.unwrap_or(color::IDENTITY);
            Some(color::mul(&ab, &color::mul(&cc, &c.color_matrix?)))
        })
    }

    /// Scene white chromaticity from AsShotNeutral / AsShotWhiteXY (else D55).
    pub fn white_xy(&self) -> [f64; 2] {
        if let Some(n) = self.as_shot_neutral {
            return self.neutral_to_xy(n);
        }
        self.as_shot_white_xy.unwrap_or([0.3324, 0.3474])
    }

    /// DNG SDK `NeutralToXY`: fixed-point iteration from D50.
    pub fn neutral_to_xy(&self, neutral: [f64; 3]) -> [f64; 2] {
        let mut xy = [0.3457, 0.3585];
        for _ in 0..30 {
            let w = calibration_weights(&self.calibrations, xy);
            let Some(m) = self.xyz_to_camera(&w).and_then(|m| color::invert(&m)) else { break };
            let next = color::xyz_to_xy(color::apply(&m, neutral));
            let done = (next[0] - xy[0]).abs() + (next[1] - xy[1]).abs() < 1e-7;
            xy = next;
            if done {
                break;
            }
        }
        xy
    }

    /// Camera colour for a scene white (DNG SDK `SetWhiteXY`).
    pub fn camera_color(&self, white: [f64; 2]) -> Option<CameraColor> {
        let w = calibration_weights(&self.calibrations, white);
        let xyz_to_cam = self.xyz_to_camera(&w)?;
        let mut cw = color::apply(&xyz_to_cam, color::xy_to_xyz(white));
        let max = cw.iter().cloned().fold(f64::MIN, f64::max);
        if max <= 0.0 {
            return None;
        }
        for c in &mut cw {
            *c = (*c / max).clamp(0.001, 1.0);
        }

        let ab = self.analog();
        let fm = blend(&self.calibrations, &w, |c| c.forward_matrix.map(normalize_forward));
        let camera_to_pcs = if let Some(fm) = fm {
            let cc = blend(&self.calibrations, &w, |c| {
                Some(c.camera_calibration.unwrap_or(color::IDENTITY))
            })?;
            let abcc_inv = color::invert(&color::mul(&ab, &cc))?;
            let ref_white = color::apply(&abcc_inv, cw);
            let d = color::diag(ref_white.map(|v| 1.0 / v.max(1e-6)));
            color::mul(&fm, &color::mul(&d, &abcc_inv))
        } else {
            let adapt = color::bradford(color::D50_XYZ, color::xy_to_xyz(white));
            let pcs_to_cam = color::mul(&xyz_to_cam, &adapt);
            let scale = color::apply(&pcs_to_cam, color::D50_XYZ).iter().cloned().fold(f64::MIN, f64::max);
            let pcs_to_cam = pcs_to_cam.map(|row| row.map(|v| v / scale));
            color::invert(&pcs_to_cam)?
        };
        let to_prophoto = color::invert(&prophoto_to_pcs())?;
        Some(CameraColor {
            camera_white: cw,
            camera_to_prophoto: color::mul(&to_prophoto, &camera_to_pcs),
            weights: w,
        })
    }

    /// HueSatMap interpolated with the colour-matrix weights.
    pub fn hue_sat_map_for(&self, weights: &[f64]) -> Option<HsvTable> {
        let base = self.hue_sat_map.as_ref()?;
        let mut data = vec![[0f32; 3]; base.data.len()];
        let mut total = 0.0;
        for (c, &w) in self.calibrations.iter().zip(weights) {
            let Some(t) = &c.hue_sat_map else { continue };
            if t.len() != data.len() || w <= 0.0 {
                continue;
            }
            for (d, s) in data.iter_mut().zip(t) {
                for k in 0..3 {
                    d[k] += (w as f32) * s[k];
                }
            }
            total += w;
        }
        if total <= 0.0 {
            return Some(base.clone());
        }
        for d in &mut data {
            for k in d.iter_mut() {
                *k /= total as f32;
            }
        }
        Some(HsvTable { data, ..base.clone() })
    }
}

/// ProPhoto -> PCS, rows rescaled so (1,1,1) maps exactly to D50.
fn prophoto_to_pcs() -> Mat3 {
    let mut m = color::PROPHOTO_TO_XYZ_D50;
    for (row, w) in m.iter_mut().zip(color::D50_XYZ) {
        let s: f64 = row.iter().sum();
        *row = row.map(|v| v * w / s);
    }
    m
}

/// DNG SDK `NormalizeForwardMatrix`: rows rescaled so FM * (1,1,1) = D50.
fn normalize_forward(mut m: Mat3) -> Mat3 {
    for (row, w) in m.iter_mut().zip(color::D50_XYZ) {
        let s: f64 = row.iter().sum();
        if s.abs() > 1e-9 {
            *row = row.map(|v| v * w / s);
        }
    }
    m
}

// ---------------------------------------------------------------------------
// Per-pixel stages.

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let v = r.max(g).max(b);
    let gap = v - r.min(g).min(b);
    if gap <= 0.0 {
        return (0.0, 0.0, v);
    }
    let h = if r == v {
        let h = (g - b) / gap;
        if h < 0.0 {
            h + 6.0
        } else {
            h
        }
    } else if g == v {
        2.0 + (b - r) / gap
    } else {
        4.0 + (r - g) / gap
    };
    (h, gap / v, v)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [v, v, v];
    }
    let h = h.rem_euclid(6.0);
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

pub fn srgb_encode(x: f32) -> f32 {
    if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

pub fn srgb_decode(y: f32) -> f32 {
    if y <= 0.003_130_8 * 12.92 {
        y / 12.92
    } else {
        ((y + 0.055) / 1.055).powf(2.4)
    }
}

/// Apply a HueSatMap/LookTable (DNG SDK `RefBaselineHueSatMap`). Values above
/// 1.0 are looked up at the table's top value row and keep their brightness.
pub fn apply_hsv_table(t: &HsvTable, rgb: [f32; 3]) -> [f32; 3] {
    let (mut h, mut s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
    if v <= 0.0 {
        return rgb;
    }
    let over = v.max(1.0);
    let vl = v.min(1.0);
    let encoded = t.srgb_encoded && t.val_divs > 1;
    let ve = if encoded { srgb_encode(vl) } else { vl };

    let h_scale = if t.hue_divs < 2 { 0.0 } else { t.hue_divs as f32 / 6.0 };
    let hs = h * h_scale;
    let ss = s * (t.sat_divs - 1) as f32;
    let vs = ve * (t.val_divs.max(2) - 1) as f32;
    let mut h0 = hs as usize;
    let mut h1 = h0 + 1;
    if h0 >= t.hue_divs - 1 {
        h0 = t.hue_divs - 1;
        h1 = 0;
    }
    let s0 = (ss as usize).min(t.sat_divs - 2);
    let v0 = if t.val_divs > 1 { (vs as usize).min(t.val_divs - 2) } else { 0 };
    let v1 = if t.val_divs > 1 { v0 + 1 } else { 0 };
    let (hf, sf) = (hs - hs.floor(), ss - s0 as f32);
    let vf = if t.val_divs > 1 { vs - v0 as f32 } else { 0.0 };
    let at = |vi: usize, hi: usize, si: usize| t.data[(vi * t.hue_divs + hi) * t.sat_divs + si];

    let mut out = [0f32; 3];
    for k in 0..3 {
        let plane = |vi| {
            let a = at(vi, h0, s0)[k] + (at(vi, h0, s0 + 1)[k] - at(vi, h0, s0)[k]) * sf;
            let b = at(vi, h1, s0)[k] + (at(vi, h1, s0 + 1)[k] - at(vi, h1, s0)[k]) * sf;
            a + (b - a) * hf
        };
        let (a, b) = (plane(v0), plane(v1));
        out[k] = a + (b - a) * vf;
    }
    let [hue_shift, sat_scale, val_scale] = out;
    h += hue_shift * (6.0 / 360.0);
    s = (s * sat_scale).min(1.0);
    let ve = (ve * val_scale).clamp(0.0, 1.0);
    let vn = if encoded { srgb_decode(ve) } else { ve };
    // Keep over-range brightness: scale the in-range result by v / 1.
    hsv_to_rgb(h, s, vn * over)
}

/// ProfileGainTableMap sampler with per-row spatial precomputation.
pub struct GainTable<'a> {
    map: &'a GainTableMap,
    weight_gain: f32,
}

impl<'a> GainTable<'a> {
    /// `baseline_exposure` is the total baseline exposure (BE + offset), which
    /// the map's input weights assume has already been applied.
    pub fn new(map: &'a GainTableMap, baseline_exposure: f64) -> Self {
        GainTable { map, weight_gain: 2f32.powf(baseline_exposure as f32) }
    }

    fn axis(pos: f64, origin: f64, spacing: f64, points: usize) -> (usize, usize, f32) {
        let f = if points <= 1 { 0.0 } else { ((pos - origin) / spacing).clamp(0.0, (points - 1) as f64) };
        let i0 = f as usize;
        (i0, (i0 + 1).min(points - 1), (f - i0 as f64) as f32)
    }

    /// Spatial index for a row, given its pixel-centred relative position.
    pub fn row(&self, v: f64) -> (usize, usize, f32) {
        Self::axis(v, self.map.origin_v, self.map.spacing_v, self.map.points_v)
    }

    pub fn col(&self, u: f64) -> (usize, usize, f32) {
        Self::axis(u, self.map.origin_h, self.map.spacing_h, self.map.points_h)
    }

    pub fn gain(&self, row: (usize, usize, f32), col: (usize, usize, f32), rgb: [f32; 3]) -> f32 {
        let m = self.map;
        let w = m.weights;
        let (mn, mx) = (rgb[0].min(rgb[1]).min(rgb[2]), rgb[0].max(rgb[1]).max(rgb[2]));
        let weight = ((w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2] + w[3] * mn + w[4] * mx)
            * self.weight_gain)
            .clamp(0.0, 1.0);
        let n = m.points_n;
        let ws = weight * n as f32;
        let w0 = (ws as usize).min(n - 1);
        let w1 = (w0 + 1).min(n - 1);
        let wf = ws - w0 as f32;
        let e = |y: usize, x: usize| {
            let base = (y * m.points_h + x) * n;
            let (a, b) = (m.gains[base + w0], m.gains[base + w1]);
            a + (b - a) * wf
        };
        let (y0, y1, yf) = row;
        let (x0, x1, xf) = col;
        let top = e(y0, x0) + (e(y0, x1) - e(y0, x0)) * xf;
        let bot = e(y1, x0) + (e(y1, x1) - e(y1, x0)) * xf;
        top + (bot - top) * yf
    }
}

/// DNG exposure ramp (`dng_function_exposure_ramp`) without the upper clamp:
/// maps `black` to 0 with a quadratic toe, and scales by `slope` above it.
#[derive(Debug, Clone, Copy)]
pub struct ExposureRamp {
    black: f32,
    slope: f32,
    radius: f32,
    qscale: f32,
}

impl ExposureRamp {
    /// `exposure` in stops (total: baseline + user); `black` the shadow black
    /// point in linear units (0 for `DefaultBlackRender = None`).
    pub fn new(exposure: f64, black: f64) -> Self {
        let white = 1.0 / 2f64.powf(exposure.max(0.0));
        let black = black.min(0.99 * white);
        // Negative exposure is a plain scale here (the SDK's SDR-only exposure
        // tone function would pin white; for HDR we keep highlights linear).
        let slope = (1.0 / (white - black)) * 2f64.powf(exposure.min(0.0));
        let radius = (0.5 * black).min((white - black) / 16.0);
        let qscale = if radius > 0.0 { slope / (4.0 * radius) } else { 0.0 };
        ExposureRamp { black: black as f32, slope: slope as f32, radius: radius as f32, qscale: qscale as f32 }
    }

    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        if x <= self.black - self.radius {
            0.0
        } else if x >= self.black + self.radius {
            (x - self.black) * self.slope
        } else {
            let d = x - (self.black - self.radius);
            self.qscale * d * d
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_round_trip() {
        for t in [2856.0, 4000.0, 5003.0, 6504.0, 7500.0] {
            let (back, tint) = xy_to_temp_tint(temp_to_xy(t));
            assert!((back - t).abs() / t < 0.01, "{t} -> {back}");
            assert!(tint.abs() < 1.0, "tint {tint}");
        }
        // D65 is ~6504 K with a small positive (green-ward) tint.
        let (t, _) = xy_to_temp_tint([0.3127, 0.3290]);
        assert!((t - 6504.0).abs() < 50.0, "D65 -> {t}");
    }

    #[test]
    fn hsv_round_trip() {
        for rgb in [[0.2, 0.5, 0.9], [0.9, 0.1, 0.4], [0.3, 0.3, 0.3]] {
            let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
            let back = hsv_to_rgb(h, s, v);
            for k in 0..3 {
                assert!((back[k] - rgb[k]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn identity_table_is_noop() {
        let t = HsvTable {
            hue_divs: 6,
            sat_divs: 2,
            val_divs: 2,
            data: vec![[0.0, 1.0, 1.0]; 24],
            srgb_encoded: true,
        };
        let out = apply_hsv_table(&t, [0.6, 0.3, 0.1]);
        for (a, b) in out.iter().zip([0.6, 0.3, 0.1]) {
            assert!((a - b).abs() < 1e-4, "{out:?}");
        }
    }

    #[test]
    fn exposure_ramp_scales_without_clipping() {
        let r = ExposureRamp::new(1.0, 0.0);
        assert!((r.apply(0.25) - 0.5).abs() < 1e-6);
        assert!((r.apply(1.0) - 2.0).abs() < 1e-6, "keeps HDR range");
        let neg = ExposureRamp::new(-1.0, 0.0);
        assert!((neg.apply(1.0) - 0.5).abs() < 1e-6);
    }
}
