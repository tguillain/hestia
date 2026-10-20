//! Sunrise and sunset, with the NOAA "sunrise equation": good to a minute or two.

/// Unix seconds of sunrise and sunset on the civil day whose noon (UTC) is
/// `noon_utc_s`, or `None` when the sun does not rise or set (polar day or night).
#[must_use]
pub fn sunrise_sunset(noon_utc_s: i64, latitude: f64, longitude: f64) -> Option<(i64, i64)> {
    #[allow(clippy::cast_precision_loss)]
    let julian_noon = noon_utc_s as f64 / 86_400.0 + 2_440_587.5;
    let days = (julian_noon - 2_451_545.0).round();
    let mean_noon = days - longitude / 360.0;
    let anomaly = (357.529_1 + 0.985_600_28 * mean_noon).rem_euclid(360.0);
    let m = anomaly.to_radians();
    let center = 1.914_8 * m.sin() + 0.02 * (2.0 * m).sin() + 0.000_3 * (3.0 * m).sin();
    let ecliptic = (anomaly + center + 180.0 + 102.937_2)
        .rem_euclid(360.0)
        .to_radians();
    let transit = 2_451_545.0 + mean_noon + 0.005_3 * m.sin() - 0.006_9 * (2.0 * ecliptic).sin();
    let declination = (ecliptic.sin() * 23.439_7_f64.to_radians().sin()).asin();
    let phi = latitude.to_radians();
    let cos_hour = ((-0.833_f64).to_radians().sin() - phi.sin() * declination.sin())
        / (phi.cos() * declination.cos());
    if !(-1.0..=1.0).contains(&cos_hour) {
        return None;
    }
    let half_day = cos_hour.acos().to_degrees() / 360.0;
    #[allow(clippy::cast_possible_truncation)]
    let unix = |julian: f64| ((julian - 2_440_587.5) * 86_400.0).round() as i64;
    Some((unix(transit - half_day), unix(transit + half_day)))
}

#[cfg(test)]
mod tests {
    use super::sunrise_sunset;

    /// 2026-06-21 12:00 UTC.
    const SOLSTICE_NOON: i64 = 1_782_043_200;

    #[test]
    fn paris_at_the_summer_solstice() {
        let (rise, set) = sunrise_sunset(SOLSTICE_NOON, 48.8566, 2.3522).unwrap();
        // Published times: 05:47 and 21:58 in Paris (UTC+2).
        let minutes = |unix: i64| (unix - (SOLSTICE_NOON - 12 * 3600)) / 60;
        assert!(
            (minutes(rise) - (3 * 60 + 47)).abs() <= 3,
            "sunrise {}",
            minutes(rise)
        );
        assert!(
            (minutes(set) - (19 * 60 + 58)).abs() <= 3,
            "sunset {}",
            minutes(set)
        );
    }

    #[test]
    fn no_sunset_at_the_pole_in_june() {
        assert_eq!(sunrise_sunset(SOLSTICE_NOON, 89.0, 0.0), None);
    }
}
