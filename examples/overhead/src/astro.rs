//! Just enough positional astronomy, after Paul Schlyter's low-precision
//! method (stjarnhimlen.se/comp/ppcomp.html). Errors are on the order of
//! arcminutes — invisible at this app's 10-degree bucket granularity.
//!
//! Everything works in degrees. `d` is the day number: days since
//! 2000 Jan 0.0 UT (J2000 epoch minus 1.5 days), fractional.

const DEG: f64 = std::f64::consts::PI / 180.0;

fn sind(x: f64) -> f64 {
    (x * DEG).sin()
}
fn cosd(x: f64) -> f64 {
    (x * DEG).cos()
}
fn atan2d(y: f64, x: f64) -> f64 {
    y.atan2(x) / DEG
}

fn rev(x: f64) -> f64 {
    x.rem_euclid(360.0)
}

/// Day number from unix milliseconds.
pub fn day_number(unix_ms: u64) -> f64 {
    let jd = unix_ms as f64 / 86_400_000.0 + 2_440_587.5;
    jd - 2_451_543.5
}

/// Greenwich mean sidereal time in degrees.
pub fn gmst_deg(unix_ms: u64) -> f64 {
    let jd = unix_ms as f64 / 86_400_000.0 + 2_440_587.5;
    rev(280.460_618_37 + 360.985_647_366_29 * (jd - 2_451_545.0))
}

/// Local sidereal time in degrees (`lon_deg` east-positive).
pub fn lst_deg(unix_ms: u64, lon_deg: f64) -> f64 {
    rev(gmst_deg(unix_ms) + lon_deg)
}

/// Angular separation between two points on the celestial sphere, degrees.
pub fn angular_sep_deg(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let c = sind(dec1) * sind(dec2) + cosd(dec1) * cosd(dec2) * cosd(ra1 - ra2);
    c.clamp(-1.0, 1.0).acos() / DEG
}

fn obliquity(d: f64) -> f64 {
    23.4393 - 3.563e-7 * d
}

/// Solve Kepler's equation, returning the eccentric anomaly in degrees.
fn kepler(m: f64, e: f64) -> f64 {
    let mut ea = m + e / DEG * sind(m) * (1.0 + e * cosd(m));
    for _ in 0..10 {
        let delta = (ea - e / DEG * sind(ea) - m) / (1.0 - e * cosd(ea));
        ea -= delta;
        if delta.abs() < 1e-8 {
            break;
        }
    }
    ea
}

/// Orbital elements at day `d`: longitude of ascending node, inclination,
/// argument of perihelion, semi-major axis, eccentricity, mean anomaly.
struct Elements {
    n: f64,
    i: f64,
    w: f64,
    a: f64,
    e: f64,
    m: f64,
}

/// Position in the orbital plane -> ecliptic rectangular coordinates
/// (units of `a`; heliocentric for planets, geocentric for the moon).
fn ecliptic_xyz(el: &Elements) -> (f64, f64, f64) {
    let ea = kepler(rev(el.m), el.e);
    let xv = el.a * (cosd(ea) - el.e);
    let yv = el.a * ((1.0 - el.e * el.e).sqrt() * sind(ea));
    let v = atan2d(yv, xv);
    let r = (xv * xv + yv * yv).sqrt();
    let vw = v + el.w;
    (
        r * (cosd(el.n) * cosd(vw) - sind(el.n) * sind(vw) * cosd(el.i)),
        r * (sind(el.n) * cosd(vw) + cosd(el.n) * sind(vw) * cosd(el.i)),
        r * sind(vw) * sind(el.i),
    )
}

/// Geocentric ecliptic rectangular -> (ra_deg, dec_deg).
fn equatorial(d: f64, x: f64, y: f64, z: f64) -> (f64, f64) {
    let ecl = obliquity(d);
    let xe = x;
    let ye = y * cosd(ecl) - z * sind(ecl);
    let ze = y * sind(ecl) + z * cosd(ecl);
    (
        rev(atan2d(ye, xe)),
        atan2d(ze, (xe * xe + ye * ye).sqrt()),
    )
}

fn sun_elements(d: f64) -> Elements {
    Elements {
        n: 0.0,
        i: 0.0,
        w: 282.9404 + 4.70935e-5 * d,
        a: 1.0,
        e: 0.016709 - 1.151e-9 * d,
        m: rev(356.0470 + 0.985_600_258_5 * d),
    }
}

/// Geocentric ecliptic position of the sun, AU.
fn sun_xyz(d: f64) -> (f64, f64, f64) {
    let el = sun_elements(d);
    let ea = kepler(el.m, el.e);
    let xv = cosd(ea) - el.e;
    let yv = (1.0 - el.e * el.e).sqrt() * sind(ea);
    let v = atan2d(yv, xv);
    let r = (xv * xv + yv * yv).sqrt();
    let lon = rev(v + el.w);
    (r * cosd(lon), r * sind(lon), 0.0)
}

/// Sun geocentric (ra_deg, dec_deg).
pub fn sun_radec(d: f64) -> (f64, f64) {
    let (x, y, z) = sun_xyz(d);
    equatorial(d, x, y, z)
}

/// Moon geocentric (ra_deg, dec_deg), with the major perturbation terms.
pub fn moon_radec(d: f64) -> (f64, f64) {
    let el = Elements {
        n: 125.1228 - 0.052_953_808_3 * d,
        i: 5.1454,
        w: 318.0634 + 0.164_357_322_3 * d,
        a: 60.2666, // earth radii; only the direction matters here
        e: 0.054900,
        m: rev(115.3654 + 13.064_992_950_9 * d),
    };
    let (x, y, z) = ecliptic_xyz(&el);
    let mut lon = rev(atan2d(y, x));
    let mut lat = atan2d(z, (x * x + y * y).sqrt());

    let sun = sun_elements(d);
    let ls = rev(sun.m + sun.w); // sun mean longitude
    let ms = sun.m; // sun mean anomaly
    let mm = el.m; // moon mean anomaly
    let lm = rev(mm + el.w + el.n); // moon mean longitude
    let dd = lm - ls; // mean elongation
    let f = lm - el.n; // argument of latitude

    lon += -1.274 * sind(mm - 2.0 * dd)
        + 0.658 * sind(2.0 * dd)
        - 0.186 * sind(ms)
        - 0.059 * sind(2.0 * mm - 2.0 * dd)
        - 0.057 * sind(mm - 2.0 * dd + ms)
        + 0.053 * sind(mm + 2.0 * dd)
        + 0.046 * sind(2.0 * dd - ms)
        + 0.041 * sind(mm - ms)
        - 0.035 * sind(dd)
        - 0.031 * sind(mm + ms)
        - 0.015 * sind(2.0 * f - 2.0 * dd)
        + 0.011 * sind(mm - 4.0 * dd);
    lat += -0.173 * sind(f - 2.0 * dd)
        - 0.055 * sind(mm - f - 2.0 * dd)
        - 0.046 * sind(mm + f - 2.0 * dd)
        + 0.033 * sind(f + 2.0 * dd)
        + 0.017 * sind(2.0 * mm + f);

    let (x, y, z) = (
        cosd(lon) * cosd(lat),
        sind(lon) * cosd(lat),
        sind(lat),
    );
    equatorial(d, x, y, z)
}

pub const PLANETS: [&str; 7] = [
    "Mercury", "Venus", "Mars", "Jupiter", "Saturn", "Uranus", "Neptune",
];

fn planet_elements(name: &str, d: f64) -> Elements {
    match name {
        "Mercury" => Elements {
            n: 48.3313 + 3.24587e-5 * d,
            i: 7.0047 + 5.0e-8 * d,
            w: 29.1241 + 1.01444e-5 * d,
            a: 0.387098,
            e: 0.205635 + 5.59e-10 * d,
            m: rev(168.6562 + 4.092_334_436_8 * d),
        },
        "Venus" => Elements {
            n: 76.6799 + 2.46590e-5 * d,
            i: 3.3946 + 2.75e-8 * d,
            w: 54.8910 + 1.38374e-5 * d,
            a: 0.723330,
            e: 0.006773 - 1.302e-9 * d,
            m: rev(48.0052 + 1.602_130_224_4 * d),
        },
        "Mars" => Elements {
            n: 49.5574 + 2.11081e-5 * d,
            i: 1.8497 - 1.78e-8 * d,
            w: 286.5016 + 2.92961e-5 * d,
            a: 1.523688,
            e: 0.093405 + 2.516e-9 * d,
            m: rev(18.6021 + 0.524_020_776_6 * d),
        },
        "Jupiter" => Elements {
            n: 100.4542 + 2.76854e-5 * d,
            i: 1.3030 - 1.557e-7 * d,
            w: 273.8777 + 1.64505e-5 * d,
            a: 5.20256,
            e: 0.048498 + 4.469e-9 * d,
            m: rev(19.8950 + 0.083_085_300_1 * d),
        },
        "Saturn" => Elements {
            n: 113.6634 + 2.38980e-5 * d,
            i: 2.4886 - 1.081e-7 * d,
            w: 339.3939 + 2.97661e-5 * d,
            a: 9.55475,
            e: 0.055546 - 9.499e-9 * d,
            m: rev(316.9670 + 0.033_444_228_2 * d),
        },
        "Uranus" => Elements {
            n: 74.0005 + 1.3978e-5 * d,
            i: 0.7733 + 1.9e-8 * d,
            w: 96.6612 + 3.0565e-5 * d,
            a: 19.18171 - 1.55e-8 * d,
            e: 0.047318 + 7.45e-9 * d,
            m: rev(142.5905 + 0.011_725_806 * d),
        },
        "Neptune" => Elements {
            n: 131.7806 + 3.0173e-5 * d,
            i: 1.7700 - 2.55e-7 * d,
            w: 272.8461 - 6.027e-6 * d,
            a: 30.05826 + 3.313e-8 * d,
            e: 0.008606 + 2.15e-9 * d,
            m: rev(260.2471 + 0.005_995_147 * d),
        },
        _ => unreachable!("unknown planet {name}"),
    }
}

/// Planet geocentric (ra_deg, dec_deg).
pub fn planet_radec(name: &str, d: f64) -> (f64, f64) {
    let (xh, yh, zh) = ecliptic_xyz(&planet_elements(name, d));
    let (xs, ys, zs) = sun_xyz(d);
    equatorial(d, xh + xs, yh + ys, zh + zs)
}

/// Rough visual magnitude for the demo; real planetary magnitudes vary
/// with phase and distance but never enough to matter at this granularity.
pub fn planet_mag(name: &str) -> f64 {
    match name {
        "Mercury" => 0.2,
        "Venus" => -4.2,
        "Mars" => 0.7,
        "Jupiter" => -2.3,
        "Saturn" => 0.6,
        "Uranus" => 5.7,
        "Neptune" => 7.9,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Schlyter's worked example date: 1990 Apr 19 00:00 UT, d = -3543
    // (JD 2448000.5). Expected values below are from the tutorial.
    const MS_TEST_DATE: u64 = 640_483_200_000;

    #[test]
    fn day_number_matches_epoch() {
        assert!((day_number(MS_TEST_DATE) - (-3543.0)).abs() < 1e-6);
    }

    #[test]
    fn sun_position_at_epoch() {
        // tutorial: RA 26.6580°, dec +11.0084°
        let (ra, dec) = sun_radec(-3543.0);
        assert!((ra - 26.658).abs() < 0.05, "sun ra {ra}");
        assert!((dec - 11.0084).abs() < 0.05, "sun dec {dec}");
    }

    #[test]
    fn moon_position_at_epoch() {
        // tutorial (geocentric, with perturbations): RA 309.5011°, dec -19.1032°
        let (ra, dec) = moon_radec(-3543.0);
        assert!((ra - 309.5011).abs() < 0.2, "moon ra {ra}");
        assert!((dec - (-19.1032)).abs() < 0.2, "moon dec {dec}");
    }

    #[test]
    fn mercury_position_at_epoch() {
        // tutorial: RA 43.2598°, dec +19.6460°
        let (ra, dec) = planet_radec("Mercury", -3543.0);
        assert!((ra - 43.2598).abs() < 0.2, "mercury ra {ra}");
        assert!((dec - 19.646).abs() < 0.2, "mercury dec {dec}");
    }

    #[test]
    fn sidereal_time_sanity() {
        // At JD 2451545.0 (2000-01-01 12:00 UT), GMST = 280.46061837 by definition.
        let ms = 946_728_000_000u64;
        assert!((gmst_deg(ms) - 280.46).abs() < 0.01);
    }
}
