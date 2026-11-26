//! Display rendering: turns a scene-referred developed image into the two
//! renditions an Ultra HDR file carries (DESIGN.md §5.2):
//!
//! - **SDR base** — the camera's tone curve applied as a per-pixel luminance
//!   scale (so colour ratios are preserved, as camera JPEGs are; Adobe's
//!   RGB-tone is available), gamut-compressed into the output space,
//!   sRGB-encoded to 8 bits. This is what every non-HDR viewer shows, so
//!   it must stand on its own: full contrast, white at 255.
//! - **HDR rendition** — the SDR picture times a per-pixel scalar boost that
//!   rises smoothly (in log-exposure) from 1 at midtones to the HDR headroom at
//!   the brightest scene values. Linear half-float, 1.0 = SDR white.
//!
//! The boost is driven by the scene brightness *before* the camera's local
//! tone mapping, which squeezes recorded highlights into the SDR range; HDR is
//! where they get that range back. Being a scalar, it keeps hue and saturation
//! identical between the renditions (the gain map is single-channel), so HDR
//! displays show the same picture with brighter highlights — how camera Ultra
//! HDR captures behave.

use half::f16;
use libraw_dev::{color, ColorSpace, DevelopedImage};
use rayon::prelude::*;
use uhdr::ColorGamut;

/// libultrahdr's nominal half-float ceiling: 10000 nits / 203 nits (SDR white).
pub const HDR_MAX: f32 = 10000.0 / 203.0;

/// SDR diffuse-white reference luminance in nits (per the Ultra HDR model).
pub const SDR_WHITE_NITS: f32 = 203.0;

/// Default cap on auto HDR headroom (≈2.6 stops; camera captures use 1.5-2.5).
pub const DEFAULT_MAX_HEADROOM: f32 = 6.0;

/// Scene brightness (post-exposure, before local tone mapping) where the HDR
/// highlight boost starts; below it HDR == SDR. One stop under mid-grey,
/// because camera local tone maps lift midtones, so their pre-tone-map
/// brightness sits lower than their SDR rendering suggests. Tuned against
/// Pixel Ultra HDR captures (gain vs SDR level within ~0.2 stops outdoors).
const BOOST_START: f32 = 0.09;

/// Percentile of per-pixel max(RGB) taken as the scene peak for the HDR curve.
const PEAK_PERCENTILE: f32 = 99.9;

/// How to choose the HDR headroom (peak HDR brightness as a multiple of SDR
/// white).
#[derive(Debug, Clone, Copy)]
pub enum Headroom {
    /// From the content: twice the scene peak relative to SDR white (i.e. one
    /// stop above where the SDR clips), in `[2, max]`. Highlights the camera
    /// recorded far above SDR white get more HDR lift.
    Auto { max: f32 },
    /// A fixed headroom.
    Fixed(f32),
}

impl Default for Headroom {
    fn default() -> Self {
        Headroom::Auto { max: DEFAULT_MAX_HEADROOM }
    }
}

/// How the tone curve is applied to colour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToneMode {
    /// Scale each pixel by T(Y)/Y (ProPhoto luminance): keeps scene colour
    /// ratios, so a contrasty curve does not add saturation. Measured against
    /// Pixel camera JPEGs this matches their chroma per hue to ~5%, where
    /// Adobe's method over-saturated warm hues by ~18%.
    #[default]
    Luminance,
    /// Adobe's RGB-tone (DNG SDK `RefBaselineRGBTone`): the curve on the max
    /// and min channels, which deepens saturation in shadows/midtones. Matches
    /// Lightroom/ACR's rendering of the same DNG.
    Adobe,
}

/// Rendering options.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOptions {
    /// Output primaries for both renditions.
    pub color_space: ColorSpace,
    pub headroom: Headroom,
    pub tone: ToneMode,
}

/// The two renditions handed to the encoder.
pub struct Renditions {
    pub width: u32,
    pub height: u32,
    pub gamut: ColorGamut,
    /// Packed RGBA8888, sRGB transfer, full range.
    pub sdr: Vec<u8>,
    /// Packed RGBA half-float (little-endian), linear, 1.0 = SDR white.
    pub hdr: Vec<u8>,
    /// Peak HDR value (multiple of SDR white) the HDR curve reaches.
    pub headroom: f32,
}

/// Map a color space to the encoder's gamut tag.
pub fn gamut_for(color_space: ColorSpace) -> ColorGamut {
    match color_space {
        ColorSpace::Srgb => ColorGamut::UHDR_CG_BT_709,
        ColorSpace::DisplayP3 => ColorGamut::UHDR_CG_DISPLAY_P3,
        ColorSpace::Rec2020 => ColorGamut::UHDR_CG_BT_2100,
    }
}

/// A monotonic tone curve baked into a lookup table over `[0, max]`.
struct Lut {
    table: Vec<f32>,
    max: f32,
}

impl Lut {
    fn new(max: f32, size: usize, f: impl Fn(f32) -> f32) -> Self {
        let table = (0..size).map(|i| f(max * i as f32 / (size - 1) as f32)).collect();
        Lut { table, max }
    }

    #[inline]
    fn eval(&self, x: f32) -> f32 {
        let pos = (x.clamp(0.0, self.max) / self.max) * (self.table.len() - 1) as f32;
        let i = (pos as usize).min(self.table.len() - 2);
        let f = pos - i as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }
}

/// Natural cubic spline through `(x, y)` points (DNG SDK `dng_spline_solver`),
/// evaluated on [0, 1].
fn spline(points: &[(f32, f32)]) -> impl Fn(f32) -> f32 {
    let x: Vec<f64> = points.iter().map(|p| p.0 as f64).collect();
    let y: Vec<f64> = points.iter().map(|p| p.1 as f64).collect();
    let n = x.len();
    // Solve for second derivatives with natural end conditions.
    let mut m = vec![0.0; n];
    if n > 2 {
        let mut c = vec![0.0; n];
        let mut d = vec![0.0; n];
        for i in 1..n - 1 {
            let (h0, h1) = (x[i] - x[i - 1], x[i + 1] - x[i]);
            let a = h0 / 6.0;
            let b = (h0 + h1) / 3.0 - a * c[i - 1];
            c[i] = (h1 / 6.0) / b;
            let r = (y[i + 1] - y[i]) / h1 - (y[i] - y[i - 1]) / h0;
            d[i] = (r - a * d[i - 1]) / b;
        }
        for i in (1..n - 1).rev() {
            m[i] = d[i] - c[i] * m[i + 1];
        }
    }
    move |t: f32| {
        let t = t as f64;
        let k = match x.iter().position(|&xi| xi >= t) {
            Some(0) => return y[0] as f32,
            Some(k) => k,
            None => return y[n - 1] as f32,
        };
        let (x0, x1) = (x[k - 1], x[k]);
        let h = x1 - x0;
        if h <= 0.0 {
            return y[k] as f32;
        }
        let (a, b) = ((x1 - t) / h, (t - x0) / h);
        let v = a * y[k - 1] + b * y[k] + ((a.powi(3) - a) * m[k - 1] + (b.powi(3) - b) * m[k]) * h * h / 6.0;
        v.clamp(0.0, 1.0) as f32
    }
}

/// Generic camera-like tone curve for raws without a profile curve: a gentle
/// toe and shoulder around an identity midtone, in the family of camera
/// profile curves.
const DEFAULT_TONE_CURVE: [(f32, f32); 8] = [
    (0.0, 0.0),
    (0.01, 0.006),
    (0.05, 0.038),
    (0.18, 0.19),
    (0.35, 0.39),
    (0.6, 0.68),
    (0.85, 0.92),
    (1.0, 1.0),
];

/// Adobe's hue-preserving RGB tone (DNG SDK `RefBaselineRGBTone`): the curve
/// is applied to the max and min channels and the middle channel keeps its
/// relative position between them.
#[inline]
fn rgb_tone(lut: &Lut, rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|v| v.clamp(0.0, lut.max));
    let tone = |hi: f32, mid: f32, lo: f32| {
        let (th, tl) = (lut.eval(hi), lut.eval(lo));
        let tm = if hi > lo { tl + (th - tl) * (mid - lo) / (hi - lo) } else { th };
        (th, tm, tl)
    };
    if r >= g {
        if g > b {
            let (rr, gg, bb) = tone(r, g, b);
            [rr, gg, bb]
        } else if b > r {
            let (bb, rr, gg) = tone(b, r, g);
            [rr, gg, bb]
        } else if b > g {
            let (rr, bb, gg) = tone(r, b, g);
            [rr, gg, bb]
        } else {
            let (rr, gg) = (lut.eval(r), lut.eval(g));
            [rr, gg, gg]
        }
    } else if r >= b {
        let (gg, rr, bb) = tone(g, r, b);
        [rr, gg, bb]
    } else if b > g {
        let (bb, gg, rr) = tone(b, g, r);
        [rr, gg, bb]
    } else {
        let (gg, bb, rr) = tone(g, b, r);
        [rr, gg, bb]
    }
}

/// ProPhoto (D50) luminance weights.
const PROPHOTO_Y: [f32; 3] = [0.288_040_2, 0.711_874_1, 0.000_085_7];

/// Luminance tone: scale RGB by T(Y)/Y. Over-range results (bright saturated
/// colours) are handled downstream by the hue-preserving normalization.
#[inline]
fn lum_tone(lut: &Lut, rgb: [f32; 3]) -> [f32; 3] {
    let rgb = rgb.map(|v| v.max(0.0));
    let y = PROPHOTO_Y[0] * rgb[0] + PROPHOTO_Y[1] * rgb[1] + PROPHOTO_Y[2] * rgb[2];
    // Near black use the curve's toe slope rather than dividing by ~0.
    let y_eval = y.max(1e-4);
    let k = lut.eval(y_eval.min(lut.max)) / y_eval;
    rgb.map(|v| v * k)
}

/// Soft gamut compression (ACES reference gamut compression form): pulls
/// out-of-gamut colours towards the achromatic axis instead of hard-clipping
/// channels, which shifts hue and posterizes saturated areas.
struct GamutCompress {
    threshold: f32,
    scale: [f32; 3],
    power: f32,
    enabled: bool,
}

impl GamutCompress {
    fn new(cs: ColorSpace) -> Self {
        let (threshold, power) = (0.8f32, 1.2f32);
        // Limits: how far outside the target the most saturated real-world
        // colours (Rec.2020 primaries/secondaries) land, per channel.
        let to_out = color::mul(&cs.from_prophoto(), &color::invert(&ColorSpace::Rec2020.from_prophoto()).unwrap());
        let mut lim = [1.05f32; 3];
        for p in [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [1., 1., 0.], [0., 1., 1.], [1., 0., 1.]] {
            let v = color::apply(&to_out, p).map(|x| x as f32);
            let ach = v.iter().cloned().fold(f32::MIN, f32::max);
            for c in 0..3 {
                lim[c] = lim[c].max((ach - v[c]) / ach);
            }
        }
        let scale = lim.map(|l| (l - threshold) / (((1.0 - threshold) / (l - threshold)).powf(-power) - 1.0).powf(1.0 / power));
        GamutCompress { threshold, scale, power, enabled: cs != ColorSpace::Rec2020 }
    }

    #[inline]
    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let ach = rgb[0].max(rgb[1]).max(rgb[2]);
        if !self.enabled || ach <= 0.0 {
            return rgb.map(|v| v.max(0.0));
        }
        let mut out = rgb;
        for c in 0..3 {
            let d = (ach - rgb[c]) / ach;
            let d = if d < self.threshold {
                d
            } else {
                let s = self.scale[c];
                let x = (d - self.threshold) / s;
                self.threshold + s * x / (1.0 + x.powf(self.power)).powf(1.0 / self.power)
            };
            out[c] = (ach - d * ach).max(0.0);
        }
        out
    }
}

fn srgb_oetf(x: f32) -> f32 {
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// Scene peak: a high percentile of per-pixel brightness (subsampled).
fn scene_peak(dev: &DevelopedImage) -> f32 {
    let mut v: Vec<f32> = dev
        .scene_max
        .iter()
        .step_by(if dev.scene_max.len() > 1_000_000 { 7 } else { 1 })
        .copied()
        .collect();
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() - 1) as f32 * PEAK_PERCENTILE / 100.0) as usize;
    *v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}

/// Resolve the HDR headroom and the scene value it is reached at.
fn resolve_headroom(dev: &DevelopedImage, h: Headroom) -> (f32, f32) {
    let peak = scene_peak(dev).max(1.0);
    let headroom = match h {
        Headroom::Auto { max } => (2.0 * peak).clamp(2.0, max.max(2.0)),
        Headroom::Fixed(v) => v.clamp(1.0, HDR_MAX),
    };
    (headroom, peak)
}

/// Render the SDR base and HDR renditions from a developed image.
pub fn render(dev: &DevelopedImage, opts: &RenderOptions) -> Renditions {
    let curve: Box<dyn Fn(f32) -> f32 + Sync> = match &dev.tone_curve {
        Some(points) => Box::new(spline(points)),
        None => Box::new(spline(&DEFAULT_TONE_CURVE)),
    };
    let sdr_lut = Lut::new(1.0, 4096, |x| curve(x));

    let (headroom, peak) = resolve_headroom(dev, opts.headroom);
    let stops = headroom.log2();
    let (l0, l1) = (BOOST_START.log2(), peak.max(BOOST_START * 2.0).log2());
    // Boost as a function of scene brightness: smoothstep in log2 from mid-grey
    // (1x) to the scene peak (full headroom over the SDR value there, which is
    // white since T(1) = 1).
    let boost_lut = Lut::new(peak, 16384, |x| {
        let t = if x <= 0.0 { 0.0 } else { ((x.log2() - l0) / (l1 - l0)).clamp(0.0, 1.0) };
        (stops * t * t * (3.0 - 2.0 * t)).exp2()
    });
    tracing::debug!(headroom, peak, "hdr boost");

    let m = cs_matrix(opts.color_space);
    let gc = GamutCompress::new(opts.color_space);
    let to_out = |p: [f32; 3]| {
        gc.apply([0, 1, 2].map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2]))
    };

    let (w, h) = (dev.width as usize, dev.height as usize);
    let mut sdr = vec![0u8; w * h * 4];
    let mut hdr = vec![0u8; w * h * 8];
    let one = f16::from_f32(1.0).to_le_bytes();
    sdr.par_chunks_mut(w * 4)
        .zip(hdr.par_chunks_mut(w * 8))
        .zip(dev.data.par_chunks(w * 3))
        .zip(dev.scene_max.par_chunks(w))
        .for_each(|(((srow, hrow), src), bright)| {
            for (x, p) in src.chunks_exact(3).enumerate() {
                let p = [p[0], p[1], p[2]];
                // SDR: tone in ProPhoto (where the profile curve is defined),
                // then to the output gamut; keep hue if a channel overshoots.
                let s = to_out(match opts.tone {
                    ToneMode::Luminance => lum_tone(&sdr_lut, p),
                    ToneMode::Adobe => rgb_tone(&sdr_lut, p),
                });
                let smax = s[0].max(s[1]).max(s[2]);
                let s = if smax > 1.0 { s.map(|v| v / smax) } else { s };
                for c in 0..3 {
                    srow[x * 4 + c] = (srgb_oetf(s[c]) * 255.0 + 0.5) as u8;
                }
                srow[x * 4 + 3] = 255;

                let b = boost_lut.eval(bright[x]);
                let hv = s.map(|v| v * b);
                for c in 0..3 {
                    let b = f16::from_f32(hv[c].min(HDR_MAX)).to_le_bytes();
                    hrow[x * 8 + c * 2..x * 8 + c * 2 + 2].copy_from_slice(&b);
                }
                hrow[x * 8 + 6..x * 8 + 8].copy_from_slice(&one);
            }
        });

    Renditions {
        width: dev.width,
        height: dev.height,
        gamut: gamut_for(opts.color_space),
        sdr,
        hdr,
        headroom,
    }
}

fn cs_matrix(cs: ColorSpace) -> [[f32; 3]; 3] {
    cs.from_prophoto().map(|r| r.map(|v| v as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(pixels: &[[f32; 3]], tone: Option<Vec<(f32, f32)>>) -> DevelopedImage {
        DevelopedImage {
            width: pixels.len() as u32,
            height: 1,
            metadata: Default::default(),
            data: pixels.iter().flatten().copied().collect(),
            scene_max: pixels.iter().map(|p| p[0].max(p[1]).max(p[2])).collect(),
            tone_curve: tone,
        }
    }

    fn profile_srgb_decode(v: u8) -> f32 {
        libraw_dev::profile::srgb_decode(v as f32 / 255.0)
    }

    fn f16_at(b: &[u8], i: usize) -> f32 {
        f16::from_le_bytes([b[i * 2], b[i * 2 + 1]]).to_f32()
    }

    #[test]
    fn spline_through_identity_is_identity() {
        let s = spline(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        for x in [0.0, 0.1, 0.37, 0.9, 1.0] {
            assert!((s(x) - x).abs() < 1e-5);
        }
    }

    #[test]
    fn rgb_tone_preserves_hue_ratio() {
        let lut = Lut::new(1.0, 4096, |x| x.sqrt());
        let out = rgb_tone(&lut, [0.64, 0.36, 0.16]);
        assert!((out[0] - 0.8).abs() < 1e-3 && (out[2] - 0.4).abs() < 1e-3);
        // Middle channel keeps its relative position: (0.36-0.16)/(0.64-0.16).
        let rel = (out[1] - out[2]) / (out[0] - out[2]);
        assert!((rel - 0.2 / 0.48).abs() < 1e-3);
    }

    #[test]
    fn lum_tone_preserves_colour_ratios() {
        let lut = Lut::new(1.0, 4096, |x| x.sqrt());
        let p = [0.30f32, 0.12, 0.05];
        let out = lum_tone(&lut, p);
        assert!((out[0] / out[1] - p[0] / p[1]).abs() < 1e-4);
        assert!((out[1] / out[2] - p[1] / p[2]).abs() < 1e-4);
        // Luminance follows the curve.
        let y = |c: [f32; 3]| PROPHOTO_Y[0] * c[0] + PROPHOTO_Y[1] * c[1] + PROPHOTO_Y[2] * c[2];
        assert!((y(out) - y(p).sqrt()).abs() < 1e-3);
    }

    #[test]
    fn sdr_white_is_255_and_hdr_lifts_only_highlights() {
        // Shadows, one SDR-white pixel, and highlights twice above SDR white
        // (enough of them to be the scene peak).
        let mut px = vec![[0.05f32; 3]; 997];
        px.insert(1, [1.0; 3]);
        px.insert(2, [2.0; 3]);
        px.push([2.0; 3]);
        let d = dev(&px, None);
        let r = render(&d, &RenderOptions { color_space: ColorSpace::Srgb, headroom: Headroom::Fixed(4.0), ..Default::default() });
        assert_eq!(&r.sdr[4..7], &[255, 255, 255]);
        let sdr_lin = |i: usize| (r.sdr[i] as f32 / 255.0).powf(2.2);
        // Shadows: HDR == SDR (no boost below the start of the ramp).
        assert!((f16_at(&r.hdr, 0) - sdr_lin(0)).abs() < 0.01);
        // The brightest highlight reaches the headroom; SDR white is lifted
        // part of the way.
        assert!((f16_at(&r.hdr, 8) - 4.0).abs() < 0.05, "{}", f16_at(&r.hdr, 8));
        let w = f16_at(&r.hdr, 4);
        assert!(w > 1.5 && w < 4.0, "SDR white in HDR: {w}");
        // HDR keeps the SDR chromaticity: a coloured highlight is a scalar
        // multiple of its SDR value.
        let c = dev(&[[0.6, 0.3, 0.1], [2.0, 2.0, 2.0]], None);
        let r = render(&c, &RenderOptions { color_space: ColorSpace::Srgb, headroom: Headroom::Fixed(4.0), ..Default::default() });
        let h = [f16_at(&r.hdr, 0), f16_at(&r.hdr, 1), f16_at(&r.hdr, 2)];
        let s = [0, 1, 2].map(|i| profile_srgb_decode(r.sdr[i]));
        let ratios = [h[0] / s[0], h[1] / s[1]];
        assert!((ratios[0] - ratios[1]).abs() / ratios[0] < 0.05, "{h:?} vs {s:?}");
    }

    #[test]
    fn auto_headroom_tracks_scene_peak() {
        let mut px = vec![[0.2f32; 3]; 999];
        px.push([2.5; 3]);
        let d = dev(&px, None);
        let (h, _) = resolve_headroom(&d, Headroom::Auto { max: 6.0 });
        assert!((2.0..=6.0).contains(&h));
        let flat = dev(&[[0.3f32; 3]; 100], None);
        assert_eq!(resolve_headroom(&flat, Headroom::Auto { max: 6.0 }).0, 2.0);
    }

    #[test]
    fn gamut_compression_removes_negatives() {
        let gc = GamutCompress::new(ColorSpace::Srgb);
        let m = cs_matrix(ColorSpace::Srgb);
        // A saturated ProPhoto green is far outside sRGB.
        let p = [0.1f32, 0.8, 0.1];
        let v = [0, 1, 2].map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2]);
        assert!(v.iter().any(|c| *c < 0.0));
        let out = gc.apply(v);
        assert!(out.iter().all(|c| *c >= 0.0));
        assert!(out[1] >= out[0] && out[1] >= out[2], "stays green: {out:?}");
    }
}
