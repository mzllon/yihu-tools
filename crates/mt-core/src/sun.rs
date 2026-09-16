//! 日出日落计算（NOAA 简化算法，纯数学、无网络依赖）。
//!
//! 参考 Wikipedia "Sunrise equation"：由平太阳正午出发，
//! 经 Equation of Time 修正得到太阳过中天时刻，
//! 再按太阳赤纬求时角，得到日出/日落。

use std::f64::consts::PI;

const RAD: f64 = PI / 180.0;

fn sin_deg(x: f64) -> f64 {
    (x * RAD).sin()
}
fn cos_deg(x: f64) -> f64 {
    (x * RAD).cos()
}
fn asin_deg(x: f64) -> f64 {
    x.asin() / RAD
}
fn acos_deg(x: f64) -> f64 {
    x.acos() / RAD
}

/// Howard Hinnant 的 days_from_civil：公历日期 → 自 1970-01-01 起的天数。
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// days_from_civil 的逆变换：天数 → (年, 月, 日)。
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// 指定日期与地点的日出/日落当地时刻（小时，含小数）。
/// 极昼/极夜（无日出或日落）返回 `None`。
pub fn sun_times(
    year: i32,
    month: u32,
    day: u32,
    lat: f64,
    lon: f64,
    tz_hours: f64,
) -> Option<(f64, f64)> {
    // 该日 0h UT 对应的儒略日；J2000（2000-01-01 12:00 UT）= 2451545.0
    let j_day0 = days_from_civil(year as i64, month, day) as f64 + 2440587.5;
    // 平太阳正午：12:00 UT 按经度东移提前（天，自 J2000 起）
    let t_noon_mean = (j_day0 - 2451545.0) + 0.5 - lon / 360.0;

    let m = (357.5291 + 0.98560028 * t_noon_mean).rem_euclid(360.0);
    let c = 1.9148 * sin_deg(m) + 0.0200 * sin_deg(2.0 * m) + 0.0003 * sin_deg(3.0 * m);
    let lambda = (m + c + 180.0 + 102.9372).rem_euclid(360.0);
    // 太阳过中天（儒略日偏移，天）
    let j_transit = t_noon_mean + 0.0053 * sin_deg(m) - 0.0069 * sin_deg(2.0 * lambda);
    // 太阳赤纬
    let declination = asin_deg(sin_deg(lambda) * sin_deg(23.4397));
    // 日出/日落时角（含 -0.833° 大气折射与太阳视半径修正）
    let cos_omega =
        (sin_deg(-0.833) - sin_deg(lat) * sin_deg(declination)) / (cos_deg(lat) * cos_deg(declination));
    if !(-1.0..=1.0).contains(&cos_omega) {
        return None; // 极昼或极夜
    }
    let omega = acos_deg(cos_omega);
    let j_rise = j_transit - omega / 360.0;
    let j_set = j_transit + omega / 360.0;

    // 儒略日偏移 → UTC 时刻（小时）：J2000 整数部分对应 12:00 UT
    let to_local_hours = |j: f64| -> f64 {
        let utc = (j + 0.5).rem_euclid(1.0) * 24.0;
        (utc + tz_hours).rem_euclid(24.0)
    };
    Some((to_local_hours(j_rise), to_local_hours(j_set)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_near(got: f64, want: f64, tol: f64, what: &str) {
        assert!(
            (got - want).abs() <= tol,
            "{what}: got {got:.2}h, want ≈{want:.2}h (tol {tol})"
        );
    }

    #[test]
    fn civil_roundtrip() {
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (2026, 9, 14), (2100, 12, 31)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn beijing_solstices() {
        // 北京（39.90N, 116.41E, UTC+8）
        let (rise, set) = sun_times(2026, 6, 21, 39.9042, 116.4074, 8.0).unwrap();
        assert_near(rise, 4.76, 0.4, "北京夏至日出");
        assert_near(set, 19.78, 0.4, "北京夏至日落");
        let (rise, set) = sun_times(2026, 12, 21, 39.9042, 116.4074, 8.0).unwrap();
        assert_near(rise, 7.53, 0.4, "北京冬至日出");
        assert_near(set, 16.86, 0.4, "北京冬至日落");
    }

    #[test]
    fn sydney_winter() {
        // 悉尼（33.87S, 151.21E, UTC+10）6 月为冬季，昼短夜长
        let (rise, set) = sun_times(2026, 6, 21, -33.8688, 151.2093, 10.0).unwrap();
        assert_near(rise, 7.0, 0.4, "悉尼冬至日出");
        assert_near(set, 16.9, 0.4, "悉尼冬至日落");
        assert!(set - rise < 10.0);
    }
}
