//! `HomeKit` speaks hue/saturation and mireds; the hub speaks RGB and kelvin.

use hestia_core::Rgb;

/// `HomeKit`'s accepted colour temperature range, in mireds (about 7140 K to 2000 K).
pub const MIN_MIRED: f64 = 140.0;
pub const MAX_MIRED: f64 = 500.0;

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

/// Full-value colour: brightness is a separate characteristic.
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

/// Approximation of a black body's colour, to show a hue for white light.
#[must_use]
pub fn kelvin_to_rgb(kelvin: f64) -> Rgb {
    let t = kelvin / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let g = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    Rgb {
        r: to_u8(r),
        g: to_u8(g),
        b: to_u8(b),
    }
}

#[must_use]
pub fn kelvin_to_mired(kelvin: f64) -> f64 {
    (1_000_000.0 / kelvin.max(1.0))
        .round()
        .clamp(MIN_MIRED, MAX_MIRED)
}

#[must_use]
pub fn mired_to_kelvin(mired: f64) -> f64 {
    (1_000_000.0 / mired.clamp(MIN_MIRED, MAX_MIRED)).round()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::{hs_to_rgb, kelvin_to_mired, mired_to_kelvin, rgb_to_hs};
    use hestia_core::Rgb;

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
    fn mireds() {
        assert!((kelvin_to_mired(2000.0) - 500.0).abs() < f64::EPSILON);
        assert!(
            (kelvin_to_mired(9000.0) - 140.0).abs() < f64::EPSILON,
            "clamped to HomeKit range"
        );
        assert!((mired_to_kelvin(250.0) - 4000.0).abs() < f64::EPSILON);
    }
}
