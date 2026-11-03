//! Small shared helpers: clock, timestamp formatting, HTML escaping.

use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

use crate::timezone::TimeZone;

const TS: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
const TS_SHORT: &[FormatItem<'static>] = format_description!("[hour]:[minute]:[second]");

/// Current time as a Unix timestamp in seconds.
pub fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn at(unix: i64, tz: TimeZone) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(tz.offset())
}

/// Formats a timestamp as `YYYY-MM-DD HH:MM:SS` in the given zone.
pub fn format_ts(unix: i64, tz: TimeZone) -> String {
    at(unix, tz).format(TS).unwrap_or_else(|_| unix.to_string())
}

/// Formats a timestamp as `HH:MM:SS` in the given zone.
pub fn format_time(unix: i64, tz: TimeZone) -> String {
    at(unix, tz)
        .format(TS_SHORT)
        .unwrap_or_else(|_| unix.to_string())
}

/// Renders a duration in seconds as a compact human string.
pub fn format_age(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Escapes text for interpolation into HTML element content or attributes.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps_in_the_given_zone() {
        // 2026-11-03T12:00:00Z
        let unix = 1_793_707_200;
        let tz = |s: &str| s.parse::<TimeZone>().unwrap();
        assert_eq!(format_ts(unix, TimeZone::UTC), "2026-11-03 12:00:00");
        assert_eq!(format_ts(unix, tz("UTC+03:00")), "2026-11-03 15:00:00");
        assert_eq!(format_ts(unix, tz("UTC-05:00")), "2026-11-03 07:00:00");
        // Half-hour zones land on the half hour.
        assert_eq!(format_ts(unix, tz("UTC+05:30")), "2026-11-03 17:30:00");
        assert_eq!(format_time(unix, tz("UTC+03:00")), "15:00:00");
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(-5), "0s");
        assert_eq!(format_age(59), "59s");
        assert_eq!(format_age(61), "1m 1s");
        assert_eq!(format_age(3_725), "1h 2m");
    }

    #[test]
    fn escaping_blocks_markup_and_attribute_breakouts() {
        assert_eq!(
            html_escape("<script>alert('x' & \"y\")</script>"),
            "&lt;script&gt;alert(&#39;x&#39; &amp; &quot;y&quot;)&lt;/script&gt;"
        );
        assert_eq!(html_escape("Команда «Мечта»"), "Команда «Мечта»");
    }
}
