//! Colour-science helpers shared by development and rendering: 3x3 matrix
//! algebra, standard RGB spaces, chromatic adaptation, and CCT estimation.
//!
//! The develop path works in linear ProPhoto RGB (ROMM, D50) because that is
//! the space DNG camera profiles (look tables, tone curves) are defined in.

pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// D50 white (the DNG profile connection space white), Y = 1.
pub const D50_XYZ: [f64; 3] = [0.964_220, 1.0, 0.825_211];

/// Linear ProPhoto (ROMM) RGB -> XYZ (D50).
pub const PROPHOTO_TO_XYZ_D50: Mat3 = [
    [0.797_674_9, 0.135_191_7, 0.031_353_4],
    [0.288_040_2, 0.711_874_1, 0.000_085_7],
    [0.0, 0.0, 0.825_210_0],
];

/// Bradford chromatic adaptation D50 -> D65.
const BRADFORD_D50_TO_D65: Mat3 = [
    [0.955_576_6, -0.023_039_3, 0.063_163_6],
    [-0.028_289_5, 1.009_941_6, 0.021_007_7],
    [0.012_298_2, -0.020_483_0, 1.329_909_8],
];

const XYZ_D65_TO_SRGB: Mat3 = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266_0, 1.876_010_8, 0.041_556_0],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];

const XYZ_D65_TO_P3: Mat3 = [
    [2.493_496_9, -0.931_383_6, -0.402_710_8],
    [-0.829_489_0, 1.762_664_1, 0.023_624_7],
    [0.035_845_8, -0.076_172_4, 0.956_884_5],
];

const XYZ_D65_TO_REC2020: Mat3 = [
    [1.716_651_2, -0.355_670_8, -0.253_366_3],
    [-0.666_684_4, 1.616_481_2, 0.015_768_5],
    [0.017_639_9, -0.042_770_6, 0.942_103_1],
];

/// Bradford cone response matrix.
const BRADFORD: Mat3 = [
    [0.895_1, 0.266_4, -0.161_4],
    [-0.750_2, 1.713_5, 0.036_7],
    [0.038_9, -0.068_5, 1.029_6],
];

/// Output (display) RGB primaries. All are D65-white spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// sRGB / BT.709 primaries.
    Srgb,
    /// Display-P3 primaries.
    DisplayP3,
    /// Rec.2020 / BT.2020 primaries.
    Rec2020,
}

impl Default for ColorSpace {
    fn default() -> Self {
        // Matches what phones (and the Ultra HDR reference renderings) emit; wide
        // enough for nearly all real-world surface colours while every HDR
        // display and modern browser handles it.
        ColorSpace::DisplayP3
    }
}

impl ColorSpace {
    fn from_xyz_d65(self) -> Mat3 {
        match self {
            ColorSpace::Srgb => XYZ_D65_TO_SRGB,
            ColorSpace::DisplayP3 => XYZ_D65_TO_P3,
            ColorSpace::Rec2020 => XYZ_D65_TO_REC2020,
        }
    }

    /// Linear ProPhoto (D50) -> this space (D65), with Bradford adaptation.
    pub fn from_prophoto(self) -> Mat3 {
        mul(&self.from_xyz_d65(), &mul(&BRADFORD_D50_TO_D65, &PROPHOTO_TO_XYZ_D50))
    }

    /// Luma weights (the Y row of the space's RGB -> XYZ matrix).
    pub fn luma_weights(self) -> [f32; 3] {
        let to_xyz = invert(&self.from_xyz_d65()).unwrap_or(IDENTITY);
        [to_xyz[1][0] as f32, to_xyz[1][1] as f32, to_xyz[1][2] as f32]
    }
}

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

pub fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub fn diag(v: [f64; 3]) -> Mat3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

pub fn lerp(a: &Mat3, b: &Mat3, t: f64) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a[i][j] * (1.0 - t) + b[i][j] * t;
        }
    }
    m
}

pub fn invert(m: &Mat3) -> Option<Mat3> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [(e * i - f * h) * inv, (c * h - b * i) * inv, (b * f - c * e) * inv],
        [(f * g - d * i) * inv, (a * i - c * g) * inv, (c * d - a * f) * inv],
        [(d * h - e * g) * inv, (b * g - a * h) * inv, (a * e - b * d) * inv],
    ])
}

/// xy chromaticity -> XYZ with Y = 1.
pub fn xy_to_xyz(xy: [f64; 2]) -> [f64; 3] {
    let y = xy[1].max(1e-6);
    [xy[0] / y, 1.0, (1.0 - xy[0] - xy[1]) / y]
}

pub fn xyz_to_xy(xyz: [f64; 3]) -> [f64; 2] {
    let s = (xyz[0] + xyz[1] + xyz[2]).max(1e-12);
    [xyz[0] / s, xyz[1] / s]
}

/// Bradford adaptation matrix mapping white `from` to white `to` (XYZ).
pub fn bradford(from: [f64; 3], to: [f64; 3]) -> Mat3 {
    let s = apply(&BRADFORD, from);
    let d = apply(&BRADFORD, to);
    let scale = diag([d[0] / s[0], d[1] / s[1], d[2] / s[2]]);
    mul(&invert(&BRADFORD).unwrap_or(IDENTITY), &mul(&scale, &BRADFORD))
}

/// Correlated colour temperature (K) of an xy chromaticity (McCamy). Accurate
/// to a few kelvin over 2000-12500 K — ample for profile interpolation.
pub fn xy_to_cct(xy: [f64; 2]) -> f64 {
    let n = (xy[0] - 0.3320) / (0.1858 - xy[1]);
    (449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33).clamp(1500.0, 50000.0)
}

/// Colour temperature of an EXIF LightSource code (DNG CalibrationIlluminant),
/// or `None` for unknown/custom illuminants.
pub fn illuminant_cct(code: u16) -> Option<f64> {
    Some(match code {
        17 | 3 => 2856.0, // Standard light A / tungsten
        24 => 3200.0,     // ISO studio tungsten
        16 => 2940.0,     // warm white fluorescent
        15 => 3450.0,     // white fluorescent
        2 | 14 => 4150.0, // fluorescent / cool white fluorescent
        18 => 4874.0,     // standard light B
        13 => 5000.0,     // day white fluorescent
        23 => 5003.0,     // D50
        1 | 4 | 9 => 5500.0, // daylight / flash / fine weather
        20 => 5503.0,     // D55
        10 => 6500.0,     // cloudy
        21 => 6504.0,     // D65
        12 => 6430.0,     // daylight fluorescent
        19 => 6774.0,     // standard light C
        11 => 7500.0,     // shade
        22 => 7504.0,     // D75
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prophoto_white_maps_to_target_white() {
        // ProPhoto white (1,1,1) must land on (1,1,1) in every D65 output space.
        for cs in [ColorSpace::Srgb, ColorSpace::DisplayP3, ColorSpace::Rec2020] {
            let w = apply(&cs.from_prophoto(), [1.0, 1.0, 1.0]);
            for c in w {
                assert!((c - 1.0).abs() < 2e-3, "{cs:?} white -> {w:?}");
            }
        }
    }

    #[test]
    fn cct_of_standard_whites() {
        assert!((xy_to_cct([0.3127, 0.3290]) - 6504.0).abs() < 60.0);
        assert!((xy_to_cct([0.4476, 0.4074]) - 2856.0).abs() < 30.0);
    }
}
