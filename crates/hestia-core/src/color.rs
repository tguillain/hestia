//! Colour conversions between the hub's RGB and what bridges and lamps speak.

use crate::Rgb;

/// Hue in degrees (0–359) and saturation in percent (0–100).
#[must_use]
pub fn rgb_to_hs(rgb: Rgb) -> (f64, f64) {
    let [r, g, b] = [rgb.r, rgb.g, rgb.b].map(|channel| f64::from(channel) / 255.0);
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let hue = if delta == 0.0 {
        0.0
    } else if (max - r).abs() < f64::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max == 0.0 { 0.0 } else { delta / max * 100.0 };
    (hue.round().rem_euclid(360.0), saturation.round())
}

/// Full-value colour: brightness is a separate point.
#[must_use]
pub fn hs_to_rgb(hue: f64, saturation: f64) -> Rgb {
    let hue = hue.rem_euclid(360.0);
    let chroma = (saturation / 100.0).clamp(0.0, 1.0);
    let x = chroma * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hue {
        h if h < 60.0 => (chroma, x, 0.0),
        h if h < 120.0 => (x, chroma, 0.0),
        h if h < 180.0 => (0.0, chroma, x),
        h if h < 240.0 => (0.0, x, chroma),
        h if h < 300.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let base = 1.0 - chroma;
    let channel = |value: f64| to_u8((value + base) * 255.0);
    Rgb {
        r: channel(r),
        g: channel(g),
        b: channel(b),
    }
}

/// CIE 1931 chromaticity of an sRGB colour, ignoring its brightness.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn rgb_to_xy(rgb: Rgb) -> (f64, f64) {
    let [r, g, b] = [rgb.r, rgb.g, rgb.b].map(|channel| linear(f64::from(channel) / 255.0));
    let x = 0.4124 * r + 0.3576 * g + 0.1805 * b;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = 0.0193 * r + 0.1192 * g + 0.9505 * b;
    let sum = x + y + z;
    if sum == 0.0 {
        // Black has no chromaticity: answer the white point.
        return (0.3127, 0.3290);
    }
    (x / sum, y / sum)
}

/// Full-value sRGB colour of a chromaticity, out-of-gamut values clipped.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn xy_to_rgb(x: f64, y: f64) -> Rgb {
    let y = y.max(1e-6);
    let (big_x, big_z) = (x / y, (1.0 - x - y) / y);
    let r = 3.2406 * big_x - 1.5372 - 0.4986 * big_z;
    let g = -0.9689 * big_x + 1.8758 + 0.0415 * big_z;
    let b = 0.0557 * big_x - 0.2040 + 1.0570 * big_z;
    let [r, g, b] = [r, g, b].map(|channel| channel.max(0.0));
    let max = r.max(g).max(b).max(1e-6);
    let channel = |value: f64| to_u8(gamma(value / max) * 255.0);
    Rgb {
        r: channel(r),
        g: channel(g),
        b: channel(b),
    }
}

fn linear(channel: f64) -> f64 {
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn gamma(channel: f64) -> f64 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn to_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::{hs_to_rgb, rgb_to_hs, rgb_to_xy, xy_to_rgb};
    use crate::Rgb;

    #[test]
    fn hue_saturation_round_trip() {
        for (hue, saturation) in [(0.0, 100.0), (120.0, 100.0), (240.0, 50.0), (30.0, 80.0)] {
            assert_eq!(rgb_to_hs(hs_to_rgb(hue, saturation)), (hue, saturation));
        }
        assert_eq!(
            rgb_to_hs(Rgb {
                r: 255,
                g: 255,
                b: 255
            }),
            (0.0, 0.0)
        );
    }

    #[test]
    fn chromaticity_round_trip() {
        for rgb in [
            Rgb { r: 255, g: 0, b: 0 },
            Rgb { r: 0, g: 255, b: 0 },
            Rgb { r: 0, g: 0, b: 255 },
            Rgb {
                r: 255,
                g: 128,
                b: 0,
            },
        ] {
            let (x, y) = rgb_to_xy(rgb);
            let back = xy_to_rgb(x, y);
            for (sent, received) in [(rgb.r, back.r), (rgb.g, back.g), (rgb.b, back.b)] {
                assert!(
                    sent.abs_diff(received) <= 2,
                    "{rgb:?} came back as {back:?}"
                );
            }
        }
        let (x, y) = rgb_to_xy(Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        assert!((x - 0.3127).abs() < 0.001 && (y - 0.3290).abs() < 0.001);
    }
}
