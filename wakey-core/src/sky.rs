//! The clock face's sky: a vertical gradient that follows the sun through
//! the day. [`sun_elevation`] gives the sun's height above the horizon for
//! a time and place; [`gradient`] turns that height into five colours (top
//! to bottom of the screen, evenly spaced): nearly black and purple at
//! night, plum and magenta in twilight, a glowing orange horizon around
//! sunrise and sunset, saturated blue by day. Mornings and evenings differ
//! a little ([`sun_rising`]). The daylight colours are sampled from macOS's
//! Solar Gradients wallpaper; night and twilight are darker and redder.

/// Zurich, the place the clock's time zone belongs to (degrees, east and
/// north positive).
pub const LATITUDE: f32 = 47.37;
pub const LONGITUDE: f32 = 8.54;

/// An sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The sky's colours at 0, 25, 50, 75 and 100 % of the screen's height.
pub type Gradient = [Rgb; 5];

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

/// True while the sun climbs (morning).
pub fn sun_rising(unix: i64, lat: f32, lon: f32) -> bool {
    sun_elevation(unix + 300, lat, lon) > sun_elevation(unix, lat, lon)
}

const fn hex(c: u32) -> Rgb {
    Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

const fn sky(c: [u32; 5]) -> Gradient {
    [hex(c[0]), hex(c[1]), hex(c[2]), hex(c[3]), hex(c[4])]
}

/// Night, below the deepest twilight key: nearly black, a dark purple
/// (more red than blue, or the panel shows it as blue) towards the bottom.
const NIGHT: Gradient = sky([0x030106, 0x07030c, 0x0e0516, 0x1a0a26, 0x2c1038]);

/// Sky colours by sun elevation (degrees) while it rises, interpolated in
/// between.
const RISING: [(f32, Gradient); 9] = [
    (-30.0, NIGHT),
    (
        -17.0,
        sky([0x06030c, 0x0c0616, 0x160a24, 0x281238, 0x461c52]),
    ),
    (
        -11.0,
        sky([0x0e0718, 0x170c26, 0x26133a, 0x421e56, 0x7a3266]),
    ),
    (
        -5.0,
        sky([0x221630, 0x2c1c3c, 0x3e2850, 0x6a3c62, 0xc8703e]),
    ),
    (0.0, sky([0x3e2c2f, 0x4b3435, 0x63423a, 0x9a5d30, 0xfe6b00])),
    (6.0, sky([0x3a3b66, 0x464672, 0x5e5a82, 0x957e81, 0xfda31e])),
    (
        12.0,
        sky([0x344699, 0x3f52ab, 0x5466c0, 0x858cc9, 0xfcc26d]),
    ),
    (
        18.0,
        sky([0x3753c4, 0x405fd5, 0x5373ed, 0x7f9af7, 0xf2d4a8]),
    ),
    (
        60.0,
        sky([0x4378fe, 0x467bfe, 0x5081fe, 0x6992fe, 0xbdbbd6]),
    ),
];

/// The same while it sets: redder, more violet.
const SETTING: [(f32, Gradient); 9] = [
    (-30.0, NIGHT),
    (
        -17.0,
        sky([0x07030c, 0x0d0616, 0x180a26, 0x2c123a, 0x4c1c50]),
    ),
    (
        -11.0,
        sky([0x10071a, 0x1a0c28, 0x2a123a, 0x481c52, 0x84305a]),
    ),
    (
        -5.0,
        sky([0x261430, 0x301a3c, 0x44244e, 0x72365a, 0xcc6438]),
    ),
    (0.0, sky([0x412839, 0x4e2f40, 0x663a45, 0x9a4e38, 0xfd5b00])),
    (6.0, sky([0x383672, 0x444080, 0x5c5291, 0x947695, 0xfe992c])),
    (
        12.0,
        sky([0x34439f, 0x3e4eb0, 0x5361c6, 0x8486d0, 0xfabc74]),
    ),
    (
        18.0,
        sky([0x3753c3, 0x405fd5, 0x5373ec, 0x7f9af7, 0xf2d4a8]),
    ),
    (
        60.0,
        sky([0x4378fe, 0x467bfe, 0x5081fe, 0x6992fe, 0xbdbbd6]),
    ),
];

/// The sky at sun elevation `elevation` (degrees), rising or setting.
pub fn gradient(elevation: f32, rising: bool) -> Gradient {
    let keys = if rising { &RISING } else { &SETTING };
    let (first, last) = (keys[0], keys[keys.len() - 1]);
    if elevation <= first.0 {
        return first.1;
    }
    if elevation >= last.0 {
        return last.1;
    }
    let i = keys
        .iter()
        .position(|k| k.0 > elevation)
        .unwrap_or(keys.len() - 1);
    let ((e0, a), (e1, b)) = (keys[i - 1], keys[i]);
    mix(a, b, (elevation - e0) / (e1 - e0))
}

fn mix(a: Gradient, b: Gradient, t: f32) -> Gradient {
    core::array::from_fn(|k| lerp(a[k], b[k], t))
}

/// The colour `t` (0–1) of the way from `a` to `b`.
fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// True if text near the bottom (85–95 % of the height: the clock's alarm
/// line) should be dark: the horizon is too light there for white text.
pub fn bottom_is_light(g: &Gradient) -> bool {
    // 90 %: 60 % of the way from the 75 % stop to the bottom.
    let Rgb(r, gr, b) = lerp(g[3], g[4], 0.6);
    0.2126 * f32::from(r) + 0.7152 * f32::from(gr) + 0.0722 * f32::from(b) > 150.0
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
        assert!(sun_rising(unix(2026, 6, 21, 6, 0), LATITUDE, LONGITUDE));
        assert!(!sun_rising(unix(2026, 6, 21, 17, 0), LATITUDE, LONGITUDE));
    }

    #[test]
    fn gradient_keys_and_between() {
        assert_eq!(gradient(-70.0, true), NIGHT);
        assert_eq!(gradient(80.0, false), SETTING[SETTING.len() - 1].1);
        assert_eq!(gradient(0.0, true), RISING[4].1);
        assert_eq!(gradient(0.0, false), SETTING[4].1);
        // Halfway between two keys, each channel halfway.
        let g = gradient(-8.0, true);
        let (a, b) = (RISING[2].1[4], RISING[3].1[4]);
        assert_eq!(g[4].0, (u16::from(a.0) + u16::from(b.0)).div_ceil(2) as u8);
    }

    #[test]
    fn dark_text_only_on_a_light_horizon() {
        assert!(!bottom_is_light(&NIGHT));
        assert!(!bottom_is_light(&gradient(-11.0, false)));
        assert!(bottom_is_light(&gradient(18.0, true)));
    }
}
