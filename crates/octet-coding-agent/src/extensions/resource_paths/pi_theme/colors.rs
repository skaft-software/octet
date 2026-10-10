//! Pi 1.0.2 perceptual theme colors, ported from packages/tui/src/{colors,oklab}.ts
//! at cd32f7725fdbddbaecdff5b1e68491563394e0ca. Keep the pinned coefficients,
//! 20-step OKLCH gamut mapping and OKHSL Halley steps, not an HSL approximation.
//! Source hashes and shared oracle vectors: extensions/octet-pi-compat/test/fixtures/theme-colors.json.
//!
//! Copyright (c) 2025 Mario Zechner; Copyright (c) 2021 Björn Ottosson.
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.

use std::sync::LazyLock;

use anyhow::{ensure, Context};
use regex::Regex;

static PATTERNS: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    let n = r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:e[+-]?[0-9]+)?";
    // ECMAScript whitespace (not Rust regex's slightly different Unicode \s).
    let s = r"[\x09-\x0d\x20\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
    [
        format!(r"(?i)^oklch\({s}*({n})(%)?{s}+({n}){s}+({n})(?:deg)?{s}*\)$"),
        format!(r"(?i)^okhsl\({s}*({n})(?:deg)?{s}+({n})(%)?{s}+({n})(%)?{s}*\)$"),
    ]
    .map(|pattern| Regex::new(&pattern).expect("constant Pi color grammar"))
});

pub(super) fn parse(color: &str) -> anyhow::Result<String> {
    let (kind, captures) = PATTERNS
        .iter()
        .enumerate()
        .find_map(|(kind, pattern)| pattern.captures(color).map(|captures| (kind, captures)))
        .context("invalid Pi perceptual color")?;
    let number = |index: usize| -> anyhow::Result<f64> {
        let value = captures[index]
            .parse::<f64>()
            .context("invalid Pi color number")?;
        ensure!(value.is_finite(), "Pi color channels must be finite");
        Ok(value)
    };
    let unit = |value: f64| -> anyhow::Result<f64> {
        ensure!(
            (0.0..=1.0).contains(&value),
            "Pi color channel must be between 0 and 1"
        );
        Ok(value)
    };
    let linear = if kind == 0 {
        let l = unit(
            number(1)?
                / if captures.get(2).is_some() {
                    100.0
                } else {
                    1.0
                },
        )?;
        let c = number(3)?;
        ensure!(c >= 0.0, "Pi chroma must not be negative");
        oklch(l, c, number(4)?)
    } else {
        let h = number(1)?;
        let s = unit(
            number(2)?
                / if captures.get(3).is_some() {
                    100.0
                } else {
                    1.0
                },
        )?;
        let l = unit(
            number(4)?
                / if captures.get(5).is_some() {
                    100.0
                } else {
                    1.0
                },
        )?;
        okhsl(h, s, l)
    };
    // Pi's okhslColor rejects non-finite RGB (e.g. chroma-stop underflow at tiny L).
    ensure!(
        linear.iter().all(|v| v.is_finite()),
        "Pi color conversion must be finite"
    );
    let (r, g, b) = linear_to_rgb(linear);
    Ok(format!("#{r:02x}{g:02x}{b:02x}"))
}

type Vector = [f64; 3];
type Rgb = (u8, u8, u8);

fn linear_to_rgb(linear: Vector) -> Rgb {
    let [r, g, b] = linear.map(|v| {
        let encoded = if v > 0.0031308 {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        } else {
            12.92 * v
        };
        (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
    });
    (r, g, b)
}

// The system-theme generator uses these same pinned conversions directly,
// without round-tripping through formatted perceptual-color strings.
pub(crate) fn okhsl_rgb(h: f64, s: f64, l: f64) -> Rgb {
    linear_to_rgb(okhsl(h, s, l))
}

pub(crate) fn oklch_rgb(l: f64, c: f64, h: f64) -> Rgb {
    linear_to_rgb(oklch(l, c, h))
}

pub(crate) fn oklab_to_okhsl_lightness(x: f64) -> f64 {
    const K1: f64 = 0.206;
    const K2: f64 = 0.03;
    const K3: f64 = (1.0 + K1) / (1.0 + K2);
    0.5 * (K3 * x - K1 + ((K3 * x - K1).powi(2) + 4.0 * K2 * K3 * x).sqrt())
}

#[allow(clippy::excessive_precision)]
fn rgb_to_lab((r, g, b): Rgb) -> Vector {
    const LINEAR_SRGB_TO_LMS: [Vector; 3] = [
        [0.4122214694707629, 0.5363325372617349, 0.0514459932675022],
        [0.2119034958178251, 0.6806995506452344, 0.1073969535369405],
        [0.0883024591900564, 0.2817188391361215, 0.6299787016738222],
    ];
    const LMS_TO_LAB: [Vector; 3] = [
        [0.210454268309314, 0.793617774702305, -0.0040720430116193],
        [1.9779985324311684, -2.42859224204858, 0.450593709617411],
        [0.0259040424655478, 0.7827717124575296, -0.8086757549230774],
    ];
    let linear = [r, g, b].map(|v| {
        let value = f64::from(v) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    let lms = LINEAR_SRGB_TO_LMS.map(|row| dot(row, linear).cbrt());
    LMS_TO_LAB.map(|row| dot(row, lms))
}

pub(crate) fn rgb_to_oklch(rgb: Rgb) -> Vector {
    let [l, a, b] = rgb_to_lab(rgb);
    [
        l,
        a.hypot(b),
        (b.atan2(a) * 180.0 / std::f64::consts::PI + 360.0) % 360.0,
    ]
}

pub(crate) fn rgb_to_okhsl(rgb: Rgb) -> Vector {
    let [l, a, b] = rgb_to_lab(rgb);
    let chroma = a.hypot(b);
    let lightness = oklab_to_okhsl_lightness(l);
    if chroma < 1e-9 || lightness <= 0.0 || lightness >= 1.0 {
        return [0.0, 0.0, lightness];
    }
    let h = (b.atan2(a) * 180.0 / std::f64::consts::PI + 360.0) % 360.0;
    let [c0, c_mid, c_max] = chroma_stops(l, a / chroma, b / chroma);
    let saturation = if chroma < c_mid {
        let k1 = 0.8 * c0;
        0.8 * (chroma / (k1 + (1.0 - k1 / c_mid) * chroma))
    } else {
        let k1 = 0.2 * c_mid.powi(2) * 1.25_f64.powi(2) / c0;
        let offset = chroma - c_mid;
        0.8 + 0.2 * (offset / (k1 + (1.0 - k1 / (c_max - c_mid)) * offset))
    };
    [h, saturation.clamp(0.0, 1.0), lightness]
}
const LAB_TO_LMS: [Vector; 3] = [
    [1.0, 0.3963377773761749, 0.2158037573099136],
    [1.0, -0.1055613458156586, -0.0638541728258133],
    [1.0, -0.0894841775298119, -1.2914855480194092],
];
// Pinned verbatim from Pi's conversion matrices; keep every digit.
#[allow(clippy::excessive_precision)]
const LMS_TO_LINEAR_SRGB: [Vector; 3] = [
    [4.0767416360759583, -3.3077115392580629, 0.2309699031821043],
    [-1.2684379732850315, 2.6097573492876882, -0.341319376002657],
    [-0.0041960761386756, -0.7034186179359362, 1.7076146940746117],
];

fn dot(row: Vector, values: Vector) -> f64 {
    row[0] * values[0] + row[1] * values[1] + row[2] * values[2]
}

fn lab_to_linear(lab: Vector) -> Vector {
    let lms = LAB_TO_LMS.map(|row| dot(row, lab).powi(3));
    LMS_TO_LINEAR_SRGB.map(|row| dot(row, lms))
}

fn hue(h: f64) -> f64 {
    ((h % 360.0) + 360.0) % 360.0
}

fn oklch(l: f64, c: f64, h: f64) -> Vector {
    let radians = hue(h) * std::f64::consts::PI / 180.0;
    let (sin, cos) = (radians.sin(), radians.cos());
    let at_chroma = |c| lab_to_linear([l, c * cos, c * sin]);
    let in_gamut = |linear: Vector| linear.iter().all(|v| *v >= -1e-7 && *v <= 1.0 + 1e-7);
    let direct = at_chroma(c);
    if in_gamut(direct) {
        return direct;
    }
    let mut linear = at_chroma(0.0);
    let (mut low, mut high) = (0.0, c);
    for _ in 0..20 {
        let chroma = (low + high) / 2.0;
        let candidate = at_chroma(chroma);
        if in_gamut(candidate) {
            low = chroma;
            linear = candidate;
        } else {
            high = chroma;
        }
    }
    linear
}

fn slopes(a: f64, b: f64) -> Vector {
    LAB_TO_LMS.map(|row| row[1] * a + row[2] * b)
}

fn max_saturation(a: f64, b: f64) -> f64 {
    let (channel, [k0, k1, k2, k3, k4]) = if -1.8817031 * a - 0.80936501 * b > 1.0 {
        (
            0,
            [1.19086277, 1.76576728, 0.59662641, 0.75515197, 0.56771245],
        )
    } else if 1.8144408 * a - 1.19445267 * b > 1.0 {
        (
            1,
            [0.73956515, -0.45954404, 0.08285427, 0.12541073, -0.14503204],
        )
    } else {
        (
            2,
            [1.35733652, -0.00915799, -1.1513021, -0.50559606, 0.00692167],
        )
    };
    let saturation = k0 + k1 * a + k2 * b + k3 * a * a + k4 * a * b;
    let slopes = slopes(a, b);
    let base = slopes.map(|k| 1.0 + saturation * k);
    let weights = LMS_TO_LINEAR_SRGB[channel];
    let f = dot(weights, base.map(|v| v.powi(3)));
    let f1 = dot(
        weights,
        std::array::from_fn(|i| 3.0 * slopes[i] * base[i].powi(2)),
    );
    let f2 = dot(
        weights,
        std::array::from_fn(|i| 6.0 * slopes[i].powi(2) * base[i]),
    );
    saturation - f * f1 / (f1 * f1 - 0.5 * f * f2)
}

fn cusp(a: f64, b: f64) -> [f64; 2] {
    let saturation = max_saturation(a, b);
    let rgb = lab_to_linear([1.0, saturation * a, saturation * b]);
    let lightness = (1.0 / rgb[0].max(rgb[1]).max(rgb[2])).cbrt();
    [lightness, lightness * saturation]
}

fn max_chroma(a: f64, b: f64, l: f64, [cusp_l, cusp_c]: [f64; 2]) -> f64 {
    if l <= cusp_l {
        return cusp_c * l / cusp_l;
    }
    let t = cusp_c * (l - 1.0) / (cusp_l - 1.0);
    let slopes = slopes(a, b);
    let lms = slopes.map(|k| l + t * k);
    let cubes = lms.map(|v| v.powi(3));
    let first = std::array::from_fn(|i| 3.0 * slopes[i] * lms[i].powi(2));
    let second = std::array::from_fn(|i| 6.0 * slopes[i].powi(2) * lms[i]);
    let steps = LMS_TO_LINEAR_SRGB.map(|row| {
        let f = dot(row, cubes) - 1.0;
        let f1 = dot(row, first);
        let f2 = dot(row, second);
        let u = f1 / (f1 * f1 - 0.5 * f * f2);
        if u >= 0.0 {
            -f * u
        } else {
            f64::MAX
        }
    });
    t + steps[0].min(steps[1]).min(steps[2])
}

fn chroma_stops(l: f64, a: f64, b: f64) -> Vector {
    let peak = cusp(a, b);
    let c_max = max_chroma(a, b, l, peak);
    let k = c_max / (l * (peak[1] / peak[0])).min((1.0 - l) * (peak[1] / (1.0 - peak[0])));
    let mid_s = 0.11516993
        + 1.0
            / (7.4477897
                + 4.1590124 * b
                + a * (-2.19557347
                    + 1.75198401 * b
                    + a * (-2.13704948 - 10.02301043 * b
                        + a * (-4.24894561 + 5.38770819 * b + 4.69891013 * a))));
    let mid_t = 0.11239642
        + 1.0
            / (1.6132032 - 0.68124379 * b
                + a * (0.40370612
                    + 0.90148123 * b
                    + a * (-0.27087943
                        + 0.6122399 * b
                        + a * (0.00299215 - 0.45399568 * b - 0.14661872 * a))));
    let c_mid = 0.9
        * k
        * (1.0 / (1.0 / (l * mid_s).powi(4) + 1.0 / ((1.0 - l) * mid_t).powi(4)))
            .sqrt()
            .sqrt();
    let c0 = (1.0 / (1.0 / (l * 0.4).powi(2) + 1.0 / ((1.0 - l) * 0.8).powi(2))).sqrt();
    [c0, c_mid, c_max]
}

fn okhsl(h: f64, s: f64, lightness: f64) -> Vector {
    const K1: f64 = 0.206;
    const K2: f64 = 0.03;
    const K3: f64 = (1.0 + K1) / (1.0 + K2);
    let l = (lightness * lightness + K1 * lightness) / (K3 * (lightness + K2));
    let mut lab = [l, 0.0, 0.0];
    if l > 0.0 && l < 1.0 && s > 0.0 {
        let angle = 2.0 * std::f64::consts::PI * hue(h) / 360.0;
        let (a, b) = (angle.cos(), angle.sin());
        let [c0, c_mid, c_max] = chroma_stops(l, a, b);
        let chroma = if s < 0.8 {
            let t = 1.25 * s;
            let k1 = 0.8 * c0;
            t * k1 / (1.0 - (1.0 - k1 / c_mid) * t)
        } else {
            let t = 5.0 * (s - 0.8);
            let k1 = 0.2 * c_mid.powi(2) * 1.25_f64.powi(2) / c0;
            c_mid + t * k1 / (1.0 - (1.0 - k1 / (c_max - c_mid)) * t)
        };
        lab = [l, chroma * a, chroma * b];
    }
    lab_to_linear(lab)
}
