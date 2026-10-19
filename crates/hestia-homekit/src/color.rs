//! `HomeKit` speaks hue/saturation and mireds; the hub speaks RGB and kelvin.

use hestia_core::Rgb;

pub use hestia_core::color::{hs_to_rgb, rgb_to_hs};

/// `HomeKit`'s accepted colour temperature range, in mireds (about 7140 K to 2000 K).
pub const MIN_MIRED: f64 = 140.0;
pub const MAX_MIRED: f64 = 500.0;

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
    use super::{kelvin_to_mired, mired_to_kelvin};

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
