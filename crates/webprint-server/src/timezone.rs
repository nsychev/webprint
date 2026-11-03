//! Time zones, written the way people write them: `UTC+06:00`.
//!
//! A venue's zone is a fixed offset from UTC, not from whatever the server
//! happens to be set to, so printouts and the web UI read the same wherever
//! the server runs.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};
use time::UtcOffset;

/// The furthest any real zone sits from UTC (UTC+14:00, Kiritimati).
const MAX_MINUTES: i32 = 18 * 60;

/// A fixed offset from UTC, in minutes east.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeZone {
    minutes: i32,
}

impl TimeZone {
    pub const UTC: Self = Self { minutes: 0 };

    /// Builds a zone from minutes east of UTC.
    pub fn from_minutes(minutes: i32) -> Result<Self, TimeZoneError> {
        if minutes.abs() > MAX_MINUTES {
            return Err(TimeZoneError(format!(
                "offset {minutes} minutes is further from UTC than any real time zone"
            )));
        }
        Ok(Self { minutes })
    }

    /// The equivalent `time` offset.
    pub fn offset(self) -> UtcOffset {
        UtcOffset::from_whole_seconds(self.minutes * 60).unwrap_or(UtcOffset::UTC)
    }
}

impl Default for TimeZone {
    fn default() -> Self {
        Self::UTC
    }
}

impl fmt::Display for TimeZone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.minutes == 0 {
            return f.write_str("UTC");
        }
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let abs = self.minutes.abs();
        write!(f, "UTC{sign}{:02}:{:02}", abs / 60, abs % 60)
    }
}

/// Why a time zone could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZoneError(String);

impl fmt::Display for TimeZoneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}; write a UTC offset such as UTC+06:00, UTC-5 or UTC",
            self.0
        )
    }
}

impl std::error::Error for TimeZoneError {}

impl FromStr for TimeZone {
    type Err = TimeZoneError;

    /// Accepts `UTC`, `Z`, `UTC+6`, `UTC+06`, `UTC+06:00`, `UTC+0600` and the
    /// same without the `UTC` prefix. `GMT` is taken as a synonym for `UTC`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.trim();
        if raw.is_empty() {
            return Err(TimeZoneError("the time zone is empty".into()));
        }
        let upper = raw.to_ascii_uppercase();

        let rest = upper
            .strip_prefix("UTC")
            .or_else(|| upper.strip_prefix("GMT"))
            .unwrap_or(&upper);
        let rest = rest.trim();
        if rest.is_empty() || rest == "Z" {
            return Ok(Self::UTC);
        }

        let (sign, digits) = match rest.split_at(1) {
            ("+", rest) => (1, rest),
            ("-", rest) => (-1, rest),
            _ => {
                return Err(TimeZoneError(format!(
                    "{raw:?} is missing a sign before the offset"
                )))
            }
        };
        let digits = digits.trim();
        if digits.is_empty() {
            return Err(TimeZoneError(format!("{raw:?} has a sign but no offset")));
        }

        let (hours, minutes) = match digits.split_once(':') {
            Some((h, m)) => (h, m),
            // Also accept the compact `+0600` form.
            None if digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit()) => {
                digits.split_at(2)
            }
            None => (digits, "0"),
        };

        let parse = |part: &str, what: &str| -> Result<i32, TimeZoneError> {
            part.trim()
                .parse::<i32>()
                .map_err(|_| TimeZoneError(format!("{raw:?} has a {what} that is not a number")))
        };
        let hours = parse(hours, "number of hours")?;
        let minutes = parse(minutes, "number of minutes")?;

        if !(0..60).contains(&minutes) {
            return Err(TimeZoneError(format!(
                "{raw:?} has {minutes} minutes, which is not between 0 and 59"
            )));
        }
        if hours < 0 {
            return Err(TimeZoneError(format!(
                "{raw:?} has two signs; write UTC-5, not UTC--5"
            )));
        }
        Self::from_minutes(sign * (hours * 60 + minutes))
    }
}

impl<'de> Deserialize<'de> for TimeZone {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_spellings_all_work() {
        let cases = [
            ("UTC", 0),
            ("utc", 0),
            ("GMT", 0),
            ("Z", 0),
            ("UTC+0", 0),
            ("UTC+6", 360),
            ("UTC+06", 360),
            ("UTC+06:00", 360),
            ("UTC+0600", 360),
            ("+06:00", 360),
            ("utc+6", 360),
            ("  UTC+6  ", 360),
            ("UTC-5", -300),
            ("UTC-05:00", -300),
            ("-05:00", -300),
            // Half-hour and quarter-hour zones are real places.
            ("UTC+05:30", 330),
            ("UTC+0530", 330),
            ("UTC+05:45", 345),
            ("UTC-03:30", -210),
            ("UTC+14:00", 840),
        ];
        for (text, minutes) in cases {
            let tz: TimeZone = text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(i32::from(tz.offset().whole_minutes()), minutes, "{text:?}");
        }
    }

    #[test]
    fn it_prints_the_way_it_reads() {
        assert_eq!(TimeZone::UTC.to_string(), "UTC");
        assert_eq!(
            TimeZone::from_minutes(360).unwrap().to_string(),
            "UTC+06:00"
        );
        assert_eq!(
            TimeZone::from_minutes(-300).unwrap().to_string(),
            "UTC-05:00"
        );
        assert_eq!(
            TimeZone::from_minutes(330).unwrap().to_string(),
            "UTC+05:30"
        );
        assert_eq!(
            TimeZone::from_minutes(-210).unwrap().to_string(),
            "UTC-03:30"
        );
        // And what it prints parses back.
        for minutes in [0, 60, -60, 330, -210, 840, -720] {
            let tz = TimeZone::from_minutes(minutes).unwrap();
            assert_eq!(tz.to_string().parse::<TimeZone>().unwrap(), tz);
        }
    }

    #[test]
    fn nonsense_is_refused_with_an_example() {
        for bad in [
            "",
            "   ",
            "Moscow",
            "UTC+",
            "UTC+x",
            "UTC6",
            "6",
            "UTC+6:70",
            "UTC+20",
            "UTC--5",
            "UTC+99:00",
        ] {
            let err = bad.parse::<TimeZone>().unwrap_err().to_string();
            assert!(err.contains("UTC+06:00"), "{bad:?} -> {err}");
        }
    }

    #[test]
    fn the_offset_matches_the_time_crate() {
        let tz: TimeZone = "UTC+05:30".parse().unwrap();
        assert_eq!(tz.offset().whole_minutes(), 330);
        assert_eq!(TimeZone::UTC.offset(), UtcOffset::UTC);
    }

    #[test]
    fn it_deserializes_from_yaml() {
        #[derive(Debug, serde::Deserialize)]
        struct Holder {
            tz: TimeZone,
        }
        let h: Holder = serde_yaml_ng::from_str("tz: UTC+06:00\n").unwrap();
        assert_eq!(h.tz.offset().whole_minutes(), 360);

        // An unquoted +06:00 is a YAML string here, but the UTC prefix is what
        // the documentation shows, because it cannot be mistaken for a number.
        let h: Holder = serde_yaml_ng::from_str("tz: \"UTC+5:45\"\n").unwrap();
        assert_eq!(h.tz.offset().whole_minutes(), 345);

        let err = serde_yaml_ng::from_str::<Holder>("tz: Europe/Moscow\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("UTC+06:00"), "{err}");
    }
}
