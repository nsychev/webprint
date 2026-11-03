//! The live status panel.
//!
//! [`render`] is pure: it turns a [`Status`] into styled lines, which the
//! caller either paints with crossterm or flattens to plain text for log mode.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{JobView, Stats};

/// Emphasis for one run of text. Mapped to colours at paint time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emphasis {
    Plain,
    Dim,
    Head,
    Ok,
    Warn,
    Err,
}

/// One line of the panel.
pub type Line = Vec<(String, Emphasis)>;

/// Everything the panel shows.
#[derive(Clone, Debug)]
pub struct Status {
    pub server: String,
    pub client_name: String,
    /// Site name and slug, once the server has told us.
    pub site: Option<(String, String)>,
    /// Printer target, or `None` for the system default.
    pub printer: Option<String>,
    pub backend: String,
    pub connected: bool,
    /// When the connection was first seen to be down.
    pub offline_since: Option<i64>,
    pub last_error: Option<String>,
    /// Jobs this client printed and failed since it started.
    pub printed: u64,
    pub failed: u64,
    /// The job being printed right now.
    pub current: Option<String>,
    pub stats: Option<Stats>,
    pub recent: Vec<JobView>,
    /// Minutes east of UTC, as reported by the server for this site.
    pub tz_minutes: i32,
}

impl Status {
    pub fn new(
        server: String,
        client_name: String,
        backend: String,
        printer: Option<String>,
    ) -> Self {
        Self {
            server,
            client_name,
            site: None,
            printer,
            backend,
            connected: false,
            offline_since: None,
            last_error: None,
            printed: 0,
            failed: 0,
            current: None,
            stats: None,
            recent: Vec::new(),
            tz_minutes: 0,
        }
    }

    /// Records a successful exchange with the server.
    pub fn mark_online(&mut self) {
        self.connected = true;
        self.offline_since = None;
        self.last_error = None;
    }

    /// Records a failed exchange, keeping the time of the first failure.
    pub fn mark_offline(&mut self, error: String, now: i64) {
        if self.connected || self.offline_since.is_none() {
            self.offline_since = Some(now);
        }
        self.connected = false;
        self.last_error = Some(error);
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Formats a duration in seconds compactly.
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

/// Reads a canonical UTC offset as the server writes it: `UTC`, `UTC+06:00`,
/// `UTC-03:30`. Anything else is treated as UTC rather than failing, since
/// this only affects how times are displayed.
pub fn parse_offset_minutes(zone: &str) -> i32 {
    let rest = match zone.trim().strip_prefix("UTC") {
        Some(rest) if !rest.is_empty() => rest,
        _ => return 0,
    };
    let (sign, digits) = match rest.split_at(1) {
        ("+", digits) => (1, digits),
        ("-", digits) => (-1, digits),
        _ => return 0,
    };
    let (hours, minutes) = digits.split_once(':').unwrap_or((digits, "0"));
    match (hours.parse::<i32>(), minutes.parse::<i32>()) {
        (Ok(h), Ok(m)) if (0..=18).contains(&h) && (0..60).contains(&m) => sign * (h * 60 + m),
        _ => 0,
    }
}

/// Formats a Unix timestamp as `HH:MM:SS`, `tz_minutes` east of UTC.
pub fn format_time(unix: i64, tz_minutes: i32) -> String {
    let offset =
        time::UtcOffset::from_whole_seconds(tz_minutes * 60).unwrap_or(time::UtcOffset::UTC);
    time::OffsetDateTime::from_unix_timestamp(unix)
        .map(|t| t.to_offset(offset))
        .ok()
        .and_then(|t| {
            t.format(&time::macros::format_description!(
                "[hour]:[minute]:[second]"
            ))
            .ok()
        })
        .unwrap_or_else(|| unix.to_string())
}

/// Width of the label column, wide enough for the longest label plus a gap.
const LABEL_WIDTH: usize = 12;

fn row(label: &str, value: impl Into<String>, emphasis: Emphasis) -> Line {
    debug_assert!(
        label.len() < LABEL_WIDTH,
        "label {label:?} overflows its column"
    );
    vec![
        (format!("  {label:<LABEL_WIDTH$}"), Emphasis::Dim),
        (value.into(), emphasis),
    ]
}

/// Builds the panel.
pub fn render(status: &Status, now: i64) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(vec![
        ("  webprint print client".to_string(), Emphasis::Head),
        (
            format!("   {}", format_time(now, status.tz_minutes)),
            Emphasis::Dim,
        ),
    ]);
    lines.push(vec![("  ".to_string(), Emphasis::Dim)]);

    // Connection.
    if status.connected {
        lines.push(row(
            "Server",
            format!("{}  connected", status.server),
            Emphasis::Ok,
        ));
    } else {
        let since = match status.offline_since {
            Some(t) => format!(
                "  disconnected since {} ({})",
                format_time(t, status.tz_minutes),
                format_age(now - t)
            ),
            None => "  connecting...".to_string(),
        };
        lines.push(row(
            "Server",
            format!("{}{since}", status.server),
            Emphasis::Err,
        ));
        if let Some(err) = &status.last_error {
            lines.push(vec![
                (" ".repeat(LABEL_WIDTH + 2), Emphasis::Dim),
                (err.clone(), Emphasis::Err),
            ]);
        }
    }

    let site = status
        .site
        .as_ref()
        .map(|(name, slug)| format!("{name} ({slug})"))
        .unwrap_or_else(|| "unknown".to_string());
    lines.push(row("Site", site, Emphasis::Plain));
    lines.push(row(
        "Printer",
        format!(
            "{}  via {}",
            status
                .printer
                .clone()
                .unwrap_or_else(|| "system default".to_string()),
            status.backend
        ),
        Emphasis::Plain,
    ));
    lines.push(row("Client", status.client_name.clone(), Emphasis::Dim));
    lines.push(vec![("  ".to_string(), Emphasis::Dim)]);

    // Counters.
    let queue = match &status.stats {
        Some(s) => format!("{} waiting, {} printing", s.queued, s.printing),
        None => "unknown".to_string(),
    };
    lines.push(row(
        "Queue",
        queue,
        match &status.stats {
            Some(s) if s.queued > 0 => Emphasis::Warn,
            _ => Emphasis::Plain,
        },
    ));
    lines.push(row(
        "This run",
        format!("{} printed, {} failed", status.printed, status.failed),
        if status.failed > 0 {
            Emphasis::Warn
        } else {
            Emphasis::Plain
        },
    ));
    if let Some(s) = &status.stats {
        lines.push(row(
            "Site total",
            format!(
                "{} printed, {} failed, {} submitted, {} pages",
                s.processed, s.failed, s.submitted, s.pages_printed
            ),
            Emphasis::Dim,
        ));
    }

    // Another client on the same token is worth knowing about: they share the
    // queue, so jobs this printer never sees are not lost, just taken.
    if let Some(stats) = &status.stats {
        let others: Vec<String> = stats
            .clients
            .iter()
            .filter(|c| c.name != status.client_name)
            .map(|c| {
                format!(
                    "{} ({} job{}, seen {} ago)",
                    c.name,
                    c.claimed,
                    if c.claimed == 1 { "" } else { "s" },
                    format_age(now - c.last_seen)
                )
            })
            .collect();
        if !others.is_empty() {
            lines.push(row("Sharing", others.join(", "), Emphasis::Warn));
        }
    }

    if let Some(current) = &status.current {
        lines.push(vec![("  ".to_string(), Emphasis::Dim)]);
        lines.push(row("Printing", current.clone(), Emphasis::Warn));
    }

    // Recent jobs.
    lines.push(vec![("  ".to_string(), Emphasis::Dim)]);
    lines.push(vec![("  Recent jobs".to_string(), Emphasis::Head)]);
    if status.recent.is_empty() {
        lines.push(vec![("    nothing yet".to_string(), Emphasis::Dim)]);
    } else {
        lines.push(vec![(
            format!(
                "    {:<6}{:<14}{:<20}{:>6}  {:<9}{}",
                "Job", "Team login", "Team", "Pages", "Status", "Time"
            ),
            Emphasis::Dim,
        )]);
        for job in &status.recent {
            let emphasis = match job.state.as_str() {
                "printed" => Emphasis::Ok,
                "failed" => Emphasis::Err,
                "printing" => Emphasis::Warn,
                _ => Emphasis::Plain,
            };
            let mut line: Line = vec![(
                format!(
                    "    {:<6}{:<14}{:<20}{:>6}  ",
                    format!("#{}", job.id),
                    truncate(&job.username, 13),
                    truncate(&job.team, 19),
                    job.pages
                ),
                Emphasis::Plain,
            )];
            line.push((format!("{:<9}", job.state), emphasis));
            line.push((
                format_time(job.created_at, status.tz_minutes),
                Emphasis::Dim,
            ));
            if let Some(err) = &job.error {
                line.push((format!("  {}", truncate(err, 40)), Emphasis::Err));
            }
            lines.push(line);
        }
    }

    lines.push(vec![("  ".to_string(), Emphasis::Dim)]);
    lines.push(vec![("  Press Ctrl-C to stop.".to_string(), Emphasis::Dim)]);
    lines
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// Flattens the panel to plain text.
pub fn plain(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|(text, _)| text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_707_200; // 2026-11-03T12:00:00Z

    fn status() -> Status {
        Status::new(
            "http://10.0.0.5:8080".into(),
            "printer-host".into(),
            "lp (CUPS)".into(),
            Some("HP_LaserJet".into()),
        )
    }

    fn stats() -> Stats {
        Stats {
            site: "main".into(),
            name: "Main Hall".into(),
            queued: 2,
            printing: 1,
            processed: 17,
            failed: 1,
            submitted: 20,
            pages_printed: 34,
            timezone: Some("UTC".into()),
            clients: Vec::new(),
            recent: Vec::new(),
        }
    }

    fn job(id: u64, username: &str, pages: usize, state: &str) -> JobView {
        JobView {
            id,
            state: state.into(),
            team: format!("Team of {username}"),
            username: username.into(),
            pages,
            created_at: NOW - 60,
            error: None,
        }
    }

    #[test]
    fn the_servers_zone_is_understood() {
        assert_eq!(parse_offset_minutes("UTC"), 0);
        assert_eq!(parse_offset_minutes("UTC+06:00"), 360);
        assert_eq!(parse_offset_minutes("UTC-05:00"), -300);
        assert_eq!(parse_offset_minutes("UTC+05:30"), 330);
        assert_eq!(parse_offset_minutes("UTC-03:30"), -210);
        // Anything unexpected shows UTC rather than refusing to draw.
        for bad in ["", "nonsense", "Europe/Moscow", "UTC+99:00", "UTC+6:99"] {
            assert_eq!(parse_offset_minutes(bad), 0, "{bad:?}");
        }
    }

    #[test]
    fn a_connected_client_shows_its_site_and_printer() {
        let mut s = status();
        s.mark_online();
        s.site = Some(("Main Hall".into(), "main".into()));
        s.stats = Some(stats());
        let text = plain(&render(&s, NOW));

        assert!(text.contains("connected"), "{text}");
        assert!(!text.contains("disconnected"), "{text}");
        assert!(text.contains("http://10.0.0.5:8080"));
        assert!(text.contains("Main Hall (main)"));
        assert!(text.contains("HP_LaserJet"));
        assert!(text.contains("lp (CUPS)"));
        assert!(text.contains("printer-host"));
        assert!(text.contains("2 waiting, 1 printing"));
    }

    #[test]
    fn a_disconnected_client_shows_since_when_and_why() {
        let mut s = status();
        s.mark_online();
        s.mark_offline("cannot reach the server".into(), NOW - 200);
        let text = plain(&render(&s, NOW));

        assert!(text.contains("disconnected since 12:... ") || text.contains("disconnected since"));
        assert!(text.contains("3m 20s"), "{text}");
        assert!(text.contains("cannot reach the server"), "{text}");
    }

    #[test]
    fn the_offline_timestamp_is_the_first_failure_not_the_latest() {
        let mut s = status();
        s.mark_online();
        s.mark_offline("first".into(), NOW - 300);
        s.mark_offline("second".into(), NOW - 10);
        assert_eq!(s.offline_since, Some(NOW - 300));
        let text = plain(&render(&s, NOW));
        assert!(text.contains("5m 0s"), "{text}");
        // The newest reason is what is displayed.
        assert!(text.contains("second"), "{text}");

        // Reconnecting clears it; a later drop starts a new clock.
        s.mark_online();
        assert_eq!(s.offline_since, None);
        s.mark_offline("again".into(), NOW);
        assert_eq!(s.offline_since, Some(NOW));
    }

    #[test]
    fn the_job_counters_are_shown() {
        let mut s = status();
        s.mark_online();
        s.printed = 17;
        s.failed = 2;
        s.stats = Some(stats());
        let text = plain(&render(&s, NOW));
        assert!(text.contains("17 printed, 2 failed"), "{text}");
        assert!(
            text.contains("17 printed, 1 failed, 20 submitted, 34 pages"),
            "{text}"
        );
    }

    #[test]
    fn recent_jobs_list_username_pages_and_status() {
        let mut s = status();
        s.mark_online();
        let mut failed = job(40, "team01", 2, "failed");
        failed.error = Some("lp: printer not responding".into());
        s.recent = vec![
            job(42, "team03", 3, "printed"),
            job(41, "team07", 1, "queued"),
            failed,
        ];

        let text = plain(&render(&s, NOW));
        assert!(text.contains("Team login"), "{text}");
        assert!(text.contains("#42"), "{text}");
        assert!(text.contains("team03"), "{text}");
        assert!(text.contains("Team of team03"), "{text}");
        assert!(text.contains("printed"), "{text}");
        assert!(text.contains("#41"), "{text}");
        assert!(text.contains("queued"), "{text}");
        assert!(text.contains("lp: printer not responding"), "{text}");
        // Pages are shown per job.
        let line = text.lines().find(|l| l.contains("#42")).unwrap();
        assert!(line.contains('3'), "{line}");
    }

    #[test]
    fn an_empty_history_says_so() {
        let mut s = status();
        s.mark_online();
        assert!(plain(&render(&s, NOW)).contains("nothing yet"));
    }

    #[test]
    fn another_client_on_the_same_token_is_named() {
        let mut s = status();
        s.mark_online();
        let mut stats = stats();
        stats.clients = vec![
            crate::api::ClientInfo {
                name: "printer-host".into(), // this client
                last_seen: NOW,
                claimed: 7,
            },
            crate::api::ClientInfo {
                name: "forgotten-laptop".into(),
                last_seen: NOW - 30,
                claimed: 4,
            },
        ];
        s.stats = Some(stats);
        let text = plain(&render(&s, NOW));
        assert!(text.contains("Sharing"), "{text}");
        assert!(text.contains("forgotten-laptop"), "{text}");
        assert!(text.contains("4 jobs"), "{text}");
        // Its own name is not listed as somebody else.
        let sharing = text.lines().find(|l| l.contains("Sharing")).unwrap();
        assert!(!sharing.contains("printer-host"), "{sharing}");
    }

    #[test]
    fn a_lone_client_says_nothing_about_sharing() {
        let mut s = status();
        s.mark_online();
        let mut stats = stats();
        stats.clients = vec![crate::api::ClientInfo {
            name: "printer-host".into(),
            last_seen: NOW,
            claimed: 7,
        }];
        s.stats = Some(stats);
        assert!(!plain(&render(&s, NOW)).contains("Sharing"));
    }

    #[test]
    fn the_job_in_flight_is_called_out() {
        let mut s = status();
        s.mark_online();
        s.current = Some("job 42 for team03, 3 pages".into());
        let text = plain(&render(&s, NOW));
        assert!(text.contains("Printing"), "{text}");
        assert!(text.contains("job 42 for team03"), "{text}");
    }

    #[test]
    fn long_names_and_errors_are_trimmed_to_fit() {
        let mut s = status();
        s.mark_online();
        let mut j = job(1, &"u".repeat(40), 1, "failed");
        j.error = Some("e".repeat(200));
        s.recent = vec![j];
        let text = plain(&render(&s, NOW));
        for line in text.lines() {
            assert!(line.chars().count() < 120, "line too wide: {line:?}");
        }
        assert!(text.contains('…'));
    }

    #[test]
    fn labels_never_run_into_their_values() {
        for label in [
            "Server",
            "Site",
            "Printer",
            "Client",
            "Queue",
            "This run",
            "Site total",
            "Printing",
        ] {
            let text: String = row(label, "VALUE", Emphasis::Plain)
                .iter()
                .map(|(t, _)| t.as_str())
                .collect();
            assert!(text.starts_with(&format!("  {label}")), "{text:?}");
            assert!(
                text.ends_with(" VALUE"),
                "label {label:?} is not padded: {text:?}"
            );
            assert_eq!(text.chars().count(), LABEL_WIDTH + 2 + "VALUE".len());
        }

        // The widest label is the one that used to collide with its value.
        let mut s = status();
        s.mark_online();
        s.stats = Some(stats());
        assert!(
            plain(&render(&s, NOW)).contains("Site total  17 printed"),
            "{}",
            plain(&render(&s, NOW))
        );
    }

    #[test]
    fn the_default_printer_is_labelled() {
        let mut s = Status::new("s".into(), "c".into(), "lp".into(), None);
        s.mark_online();
        assert!(plain(&render(&s, NOW)).contains("system default"));
    }

    #[test]
    fn timestamps_respect_the_offset() {
        assert_eq!(format_time(NOW, 0), "12:00:00");
        assert_eq!(format_time(NOW, 180), "15:00:00");
        assert_eq!(format_time(NOW, 330), "17:30:00");
        assert_eq!(format_time(NOW, -300), "07:00:00");
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(3_725), "1h 2m");
    }
}
