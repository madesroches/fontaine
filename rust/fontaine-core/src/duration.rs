//! Parsing of the short duration forms a person types: `30s`, `5min`, `6h`, `2d`.

use std::time::Duration;

/// Seconds in one of the unit words a duration may end in.
pub fn unit_seconds(unit: &str) -> Option<u64> {
    Some(match unit.to_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        _ => return None,
    })
}

/// `5m`, `2h`, `30s`, `5min`: digits then a unit. Returns the count, the unit's seconds and the
/// token as written.
pub fn compact_interval(token: &str) -> Option<(u64, u64, String)> {
    let split = token.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = token.split_at(split);
    if digits.is_empty() {
        return None;
    }
    let seconds = unit_seconds(unit)?;
    Some((digits.parse().ok()?, seconds, token.to_string()))
}

/// A whole duration in the compact form. A bare number is seconds, so the flag and the config
/// key agree on `21600`.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    if let Ok(secs) = text.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let (count, seconds, _) = compact_interval(text)?;
    Some(Duration::from_secs(count.checked_mul(seconds)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_forms_parse() {
        assert_eq!(parse_duration("6h"), Some(Duration::from_secs(21_600)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5min"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("2d"), Some(Duration::from_secs(172_800)));
    }

    #[test]
    fn garbage_is_refused() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("h"), None);
        assert_eq!(parse_duration("5 fortnights"), None);
        assert_eq!(parse_duration("99999999999999999999h"), None);
    }
}
