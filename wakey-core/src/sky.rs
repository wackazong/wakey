//! The clock face's sky: a vertical gradient that follows the sun through
//! the day. [`sun_elevation`] gives the sun's height above the horizon for
//! a time and place; [`gradient`] turns that height into three colours
//! (top, middle, bottom of the screen): deep blue at night, indigo and
//! violet in twilight, warm light low down around sunrise and sunset, blue
//! by day.

/// Zurich, the place the clock's time zone belongs to (degrees, east and
/// north positive).
pub const LATITUDE: f32 = 47.37;
pub const LONGITUDE: f32 = 8.54;

/// An sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Top, middle and bottom colour of the sky.
pub type Gradient = [Rgb; 3];

/// The sun's elevation in degrees (negative: below the horizon) at Unix
/// time `unix` seen from `lat`/`lon`, accurate to a fraction of a degree
/// (NOAA's approximation; refraction ignored).
pub fn sun_elevation(unix: i64, lat: f32, lon: f32) -> f32 {
    use core::f64::consts::PI;
    let days = unix as f64 / 86_400.0;
    // Day of the year (0-based) and fraction of the day, UTC.
    let date = crate::datetime::DateTime::from_unix(unix);
    let year_start = crate::datetime::DateTime {
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
        ..date
    }
    .to_unix() as f64
        / 86_400.0;
    let day_of_year = days - year_start;
    let hours = days.rem_euclid(1.0) * 24.0;
    // Fractional year, radians.
    let g = 2.0 * PI / 365.0 * (day_of_year + (hours - 12.0) / 24.0);
    let eq_time = 229.18
        * (0.000075 + 0.001868 * g.cos()
            - 0.032077 * g.sin()
            - 0.014615 * (2.0 * g).cos()
            - 0.040849 * (2.0 * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos()
        + 0.000907 * (2.0 * g).sin()
        - 0.002697 * (3.0 * g).cos()
        + 0.00148 * (3.0 * g).sin();
    // True solar time, minutes; hour angle, radians.
    let solar_min = hours * 60.0 + eq_time + 4.0 * f64::from(lon);
    let hour_angle = (solar_min / 4.0 - 180.0).to_radians();
    let lat = f64::from(lat).to_radians();
    let cos_zenith = lat.sin() * decl.sin() + lat.cos() * decl.cos() * hour_angle.cos();
    (90.0 - cos_zenith.clamp(-1.0, 1.0).acos().to_degrees()) as f32
}

/// Sky colours by sun elevation (degrees), interpolated in between.
const KEYS: [(f32, Gradient); 11] = [
    // Night.
    (-18.0, [Rgb(4, 6, 14), Rgb(8, 12, 26), Rgb(13, 20, 38)]),
    // Astronomical and nautical twilight: the blue comes back.
    (-12.0, [Rgb(6, 10, 30), Rgb(14, 22, 50), Rgb(26, 32, 66)]),
    (-8.0, [Rgb(10, 20, 48), Rgb(28, 34, 80), Rgb(58, 44, 92)]),
    // Civil twilight: violet, the horizon warms.
    (-4.0, [Rgb(18, 34, 76), Rgb(56, 56, 112), Rgb(120, 72, 106)]),
    (
        -1.0,
        [Rgb(30, 56, 108), Rgb(104, 88, 138), Rgb(204, 118, 96)],
    ),
    // Sunrise, sunset.
    (
        2.0,
        [Rgb(42, 82, 142), Rgb(138, 124, 156), Rgb(232, 158, 104)],
    ),
    (
        6.0,
        [Rgb(50, 98, 162), Rgb(122, 152, 192), Rgb(226, 186, 148)],
    ),
    // Day.
    (
        12.0,
        [Rgb(50, 108, 176), Rgb(104, 150, 202), Rgb(178, 202, 220)],
    ),
    (
        25.0,
        [Rgb(44, 104, 180), Rgb(88, 144, 204), Rgb(154, 192, 226)],
    ),
    (
        45.0,
        [Rgb(40, 98, 176), Rgb(78, 134, 200), Rgb(138, 182, 226)],
    ),
    (
        90.0,
        [Rgb(38, 94, 172), Rgb(74, 128, 196), Rgb(132, 176, 222)],
    ),
];

/// The sky at sun elevation `elevation` (degrees).
pub fn gradient(elevation: f32) -> Gradient {
    let (first, last) = (KEYS[0], KEYS[KEYS.len() - 1]);
    if elevation <= first.0 {
        return first.1;
    }
    if elevation >= last.0 {
        return last.1;
    }
    let i = KEYS
        .iter()
        .position(|k| k.0 > elevation)
        .unwrap_or(KEYS.len() - 1);
    let ((e0, a), (e1, b)) = (KEYS[i - 1], KEYS[i]);
    let t = (elevation - e0) / (e1 - e0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    core::array::from_fn(|k| {
        Rgb(
            mix(a[k].0, b[k].0),
            mix(a[k].1, b[k].1),
            mix(a[k].2, b[k].2),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::DateTime;

    fn unix(y: u16, mo: u8, d: u8, h: u8, mi: u8) -> i64 {
        DateTime {
            year: y,
            month: mo,
            day: d,
            hour: h,
            minute: mi,
            second: 0,
            weekday: 0,
        }
        .to_unix()
    }

    #[test]
    fn sun_over_zurich() {
        let at = |t| sun_elevation(t, LATITUDE, LONGITUDE);
        // Summer solstice, solar noon (~11:25 UTC): 90 − 47.4 + 23.4 ≈ 66°.
        let noon = at(unix(2026, 6, 21, 11, 25));
        assert!((noon - 66.0).abs() < 1.0, "{noon}");
        // Winter solstice noon (~11:22 UTC): ≈ 19°.
        let winter = at(unix(2026, 12, 21, 11, 22));
        assert!((winter - 19.2).abs() < 1.0, "{winter}");
        // Midnight is night; sunrise on 21 June is ~03:30 UTC (05:30 CEST).
        assert!(at(unix(2026, 6, 21, 23, 25)) < -18.0);
        let sunrise = at(unix(2026, 6, 21, 3, 30));
        assert!(sunrise.abs() < 1.5, "{sunrise}");
    }

    #[test]
    fn gradient_ends_and_middle() {
        assert_eq!(gradient(-40.0), KEYS[0].1);
        assert_eq!(gradient(95.0), KEYS[KEYS.len() - 1].1);
        assert_eq!(gradient(2.0), KEYS[5].1);
        // Halfway between two keys, each channel halfway.
        let g = gradient(-10.0);
        let (a, b) = (KEYS[1].1[0], KEYS[2].1[0]);
        assert_eq!(g[0].2, ((u16::from(a.2) + u16::from(b.2)) / 2) as u8);
    }
}
