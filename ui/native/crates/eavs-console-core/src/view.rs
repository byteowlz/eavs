//! Presentation rules shared by every console surface: how a meter reads,
//! when a window resets, how full a bar is. Pure and tested.

use chrono::{DateTime, Utc};

use crate::client::Meter;

/// Display text for a meter's value. Unknown reads "No data", never 0.
pub fn meter_value(meter: &Meter) -> String {
    let money = |v: f64| format!("${v:.2}");
    match meter.unit.as_str() {
        "percent" => meter
            .used
            .map_or_else(|| "No data".to_owned(), |used| format!("{}%", used.round())),
        "usd" => match (meter.used, meter.limit, meter.remaining) {
            (Some(used), Some(limit), _) => format!("{} of {}", money(used), money(limit)),
            (_, _, Some(remaining)) => format!("{} left", money(remaining)),
            (Some(used), None, None) => format!("{} spent", money(used)),
            _ => "No data".to_owned(),
        },
        _ => match (meter.used, meter.limit, meter.remaining) {
            (Some(used), Some(limit), _) => format!("{} of {}", trim(used), trim(limit)),
            (_, _, Some(remaining)) => format!("{} left", trim(remaining)),
            (Some(used), None, None) => trim(used),
            _ => "No data".to_owned(),
        },
    }
}

fn trim(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

/// Fill fraction 0..=1 for a bar, when the meter has a limit.
pub fn meter_fraction(meter: &Meter) -> Option<f32> {
    let limit = meter.limit.filter(|l| *l > 0.0)?;
    let used = meter.used.or_else(|| meter.remaining.map(|r| limit - r))?;
    #[expect(clippy::cast_possible_truncation, reason = "display fraction")]
    Some((used / limit).clamp(0.0, 1.0) as f32)
}

/// Severity for colouring a bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    High,
    Full,
}

pub fn meter_level(meter: &Meter) -> Level {
    match meter_fraction(meter) {
        Some(f) if f >= 0.999 => Level::Full,
        Some(f) if f >= 0.8 => Level::High,
        _ => Level::Normal,
    }
}

/// "resets in 3h 12m", "resets in 2d", "resetting", or empty.
pub fn reset_text(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(at) = resets_at else {
        return String::new();
    };
    let seconds = (at - now).num_seconds();
    if seconds <= 0 {
        return "resetting".to_owned();
    }
    let minutes = seconds.div_euclid(60);
    let hours = minutes / 60;
    let days = hours / 24;
    if days >= 2 {
        format!("resets in {days}d")
    } else if hours >= 1 {
        format!("resets in {hours}h {}m", minutes % 60)
    } else {
        format!("resets in {}m", minutes.max(1))
    }
}

/// Pace: how far through its window the meter is, versus how much is used.
/// Positive means using faster than the window elapses.
pub fn pace(meter: &Meter, now: DateTime<Utc>) -> Option<f32> {
    let fraction = meter_fraction(meter)?;
    let period = i64::try_from(meter.period_secs?).ok()?;
    let at = meter.resets_at?;
    let remaining = (at - now).num_seconds().clamp(0, period);
    #[expect(clippy::cast_precision_loss, reason = "display ratio")]
    let elapsed = 1.0 - remaining as f32 / period as f32;
    Some(fraction - elapsed)
}

/// Relative "read 3 min ago" text.
pub fn ago(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(at) = at else {
        return "not read yet".to_owned();
    };
    let minutes = (now - at).num_minutes();
    match minutes {
        i64::MIN..=0 => "just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        _ => format!("{} h ago", minutes / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn meter(unit: &str, used: Option<f64>, limit: Option<f64>, remaining: Option<f64>) -> Meter {
        Meter {
            id: "m".into(),
            label: "M".into(),
            unit: unit.into(),
            used,
            limit,
            remaining,
            resets_at: None,
            period_secs: None,
        }
    }

    #[test]
    fn values_read_naturally_and_unknown_is_not_zero() {
        assert_eq!(
            meter_value(&meter("percent", Some(23.4), Some(100.0), None)),
            "23%"
        );
        assert_eq!(
            meter_value(&meter("percent", None, Some(100.0), None)),
            "No data"
        );
        assert_eq!(
            meter_value(&meter("usd", Some(12.34), Some(50.0), None)),
            "$12.34 of $50.00"
        );
        assert_eq!(
            meter_value(&meter("usd", None, None, Some(0.0))),
            "$0.00 left"
        );
        assert_eq!(
            meter_value(&meter("credits", None, None, Some(821.0))),
            "821 left"
        );
    }

    #[test]
    fn fractions_and_levels() {
        let full = meter("percent", Some(100.0), Some(100.0), None);
        assert_eq!(meter_level(&full), Level::Full);
        let hot = meter("usd", None, Some(50.0), Some(5.0));
        assert_eq!(meter_fraction(&hot), Some(0.9));
        assert_eq!(meter_level(&hot), Level::High);
        assert_eq!(
            meter_fraction(&meter("credits", None, None, Some(3.0))),
            None
        );
    }

    #[test]
    fn reset_countdowns() {
        let now = Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap();
        let at =
            |h: i64, m: i64| Some(now + chrono::Duration::hours(h) + chrono::Duration::minutes(m));
        assert_eq!(reset_text(at(3, 12), now), "resets in 3h 12m");
        assert_eq!(reset_text(at(0, 7), now), "resets in 7m");
        assert_eq!(reset_text(at(72, 0), now), "resets in 3d");
        assert_eq!(reset_text(at(-1, 0), now), "resetting");
        assert_eq!(reset_text(None, now), "");
    }

    #[test]
    fn pace_compares_usage_with_elapsed_window() {
        let now = Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap();
        let mut m = meter("percent", Some(50.0), Some(100.0), None);
        m.period_secs = Some(10 * 3600);
        m.resets_at = Some(now + chrono::Duration::hours(8)); // 20% elapsed
        let pace = pace(&m, now).unwrap();
        assert!((pace - 0.3).abs() < 0.001);
    }
}
