//! Server configuration: parsing, validation and lookups.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::timezone::TimeZone;

/// Top-level configuration, loaded from YAML.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Socket address to bind, e.g. `0.0.0.0:8080`.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Public path prefix when the server is proxied below the root, e.g.
    /// `/print` for `https://example.org/print/`. Links, redirects and the
    /// session cookie are scoped to it, and the routes answer both with and
    /// without the prefix, so it works whether or not the proxy strips it.
    #[serde(default)]
    pub base_path: Option<String>,
    /// Where to keep the queue on disk, so a restart does not lose it. The
    /// queue stays in memory either way; this is a record of it. Defaults to
    /// `webprint-state.jsonl` beside the configuration file.
    #[serde(default)]
    pub state_file: Option<PathBuf>,
    /// Token guarding the cross-site stats pages. Stats are unavailable
    /// without it (per-site stats still work with the site token).
    #[serde(default)]
    pub admin_token: Option<String>,
    /// Key used to sign session cookies. A random key is generated when
    /// omitted, which invalidates existing sessions on restart.
    #[serde(default)]
    pub secret: Option<String>,
    /// Session lifetime in hours.
    #[serde(default = "default_session_hours")]
    pub session_hours: i64,
    /// Time zone for the times shown to humans and printed on the page,
    /// written as a UTC offset: `UTC+06:00`, `UTC-5`, `UTC`. A site may
    /// override it; see [`SiteConfig::timezone`].
    #[serde(default)]
    pub timezone: Option<TimeZone>,
    /// Removed in favour of `timezone`. Kept only to explain the change.
    #[serde(default)]
    pub timezone_offset_hours: Option<i8>,
    /// Set the `Secure` flag on cookies (only works when served over HTTPS).
    #[serde(default)]
    pub secure_cookies: bool,
    #[serde(default)]
    pub pdf: PdfConfig,
    #[serde(default)]
    pub limits: Limits,
    /// Contest sites, keyed by slug.
    pub sites: BTreeMap<String, SiteConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    /// Human-readable site name; defaults to the slug.
    #[serde(default)]
    pub name: Option<String>,
    /// Time zone for this venue, as a UTC offset such as `UTC+06:00`. When
    /// set it overrides the server-wide zone for this site's printouts and
    /// for what its teams see, which is what a contest spanning time zones
    /// needs.
    #[serde(default)]
    pub timezone: Option<TimeZone>,
    /// Removed in favour of `timezone`. Kept only to explain the change.
    #[serde(default)]
    pub timezone_offset_hours: Option<i8>,
    /// Bearer token the onsite print client authenticates with.
    pub token: String,
    pub teams: Vec<TeamConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamConfig {
    pub username: String,
    pub password: String,
    /// Team name, printed on the page header.
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfConfig {
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    #[serde(default = "default_line_spacing")]
    pub line_spacing: f64,
    #[serde(default = "default_tab_width")]
    pub tab_width: usize,
    /// Prefix every line with its number.
    #[serde(default = "default_true")]
    pub line_numbers: bool,
    /// Apply syntax highlighting (bold keywords, italic comments) to code.
    #[serde(default = "default_true")]
    pub highlight: bool,
    /// Directory of extra `.sublime-syntax` grammars to load, for languages
    /// the bundled set lacks (Kotlin, for instance).
    #[serde(default)]
    pub syntax_dir: Option<PathBuf>,
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
    /// Override the bundled monospace font.
    #[serde(default)]
    pub font_path: Option<PathBuf>,
    #[serde(default)]
    pub font_bold_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Jobs a single team may have waiting at once.
    #[serde(default = "default_max_queued_per_team")]
    pub max_queued_per_team: usize,
    /// Finished jobs retained per site for status display and stats.
    #[serde(default = "default_job_history")]
    pub job_history: usize,
    /// How long a claimed job may stay unconfirmed before being requeued.
    #[serde(default = "default_lease_seconds")]
    pub lease_seconds: u64,
    /// Delivery attempts before a job is marked failed.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// How long `claim` holds a request open waiting for work (long polling).
    #[serde(default = "default_poll_wait_seconds")]
    pub poll_wait_seconds: u64,
}

fn default_true() -> bool {
    true
}
fn default_listen() -> String {
    "0.0.0.0:8080".to_string()
}
fn default_session_hours() -> i64 {
    24
}
fn default_font_size() -> f64 {
    10.0
}
fn default_line_spacing() -> f64 {
    // Matches webprint_pdf::Style::default, so a configuration that leaves it
    // out lays out exactly like one that spells it out.
    1.25
}
fn default_tab_width() -> usize {
    4
}
fn default_max_pages() -> usize {
    10
}
fn default_max_queued_per_team() -> usize {
    3
}
fn default_job_history() -> usize {
    200
}
fn default_lease_seconds() -> u64 {
    120
}
fn default_max_attempts() -> u32 {
    3
}
fn default_poll_wait_seconds() -> u64 {
    25
}

impl Default for PdfConfig {
    fn default() -> Self {
        serde_yaml_ng::from_str("{}").expect("all pdf fields have defaults")
    }
}

impl Default for Limits {
    fn default() -> Self {
        serde_yaml_ng::from_str("{}").expect("all limit fields have defaults")
    }
}

/// Normalizes a public path prefix: `""`, `"/"` and `"/print/"` become `""`,
/// `""` and `"/print"` respectively.
pub fn normalize_base_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// Which team a set of credentials belongs to.
///
/// A login and password together identify exactly one team: the configuration
/// is rejected if any pair is listed twice, so there is no ambiguous case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    Ok {
        site: String,
        username: String,
    },
    /// No team anywhere has this username and password.
    Invalid,
}

impl Config {
    /// Reads and validates a configuration file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    pub fn parse_str(text: &str) -> Result<Self> {
        let config: Self = serde_yaml_ng::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Rejects configurations that would misbehave at runtime.
    pub fn validate(&self) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();

        if let Some(raw) = &self.base_path {
            if !raw.trim().is_empty()
                && !raw
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~'))
            {
                errors.push(format!(
                    "base_path {raw:?}: use only letters, digits, '/', '-', '_', '.' and '~'"
                ));
            }
        }

        if self.sites.is_empty() {
            errors.push("no sites configured".into());
        }
        if self.session_hours <= 0 {
            errors.push("session_hours must be positive".into());
        }
        if let Some(hours) = self.timezone_offset_hours {
            errors.push(format!(
                "timezone_offset_hours has been replaced by timezone: write \
                 `timezone: UTC{}{:02}:00` instead",
                if hours < 0 { '-' } else { '+' },
                hours.abs()
            ));
        }
        if self.pdf.font_size <= 1.0 {
            errors.push("pdf.font_size must be greater than 1".into());
        }
        if self.pdf.line_spacing < 1.0 {
            errors.push("pdf.line_spacing must be at least 1.0".into());
        }
        if self.pdf.tab_width == 0 {
            errors.push("pdf.tab_width must be positive".into());
        }
        if self.pdf.max_pages == 0 {
            errors.push("pdf.max_pages must be positive".into());
        }
        if self.limits.max_queued_per_team == 0 {
            errors.push("limits.max_queued_per_team must be positive".into());
        }
        if self.limits.lease_seconds == 0 {
            errors.push("limits.lease_seconds must be positive".into());
        }
        if self.limits.max_attempts == 0 {
            errors.push("limits.max_attempts must be positive".into());
        }
        if self.admin_token.as_deref().is_some_and(str::is_empty) {
            errors.push("admin_token must not be empty when set".into());
        }

        // A login and password together have to identify one team, so the
        // same pair may not appear twice anywhere - within a site or across
        // them. Teams are told apart by the pair alone.
        let mut credentials: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        for (slug, site) in &self.sites {
            for team in &site.teams {
                let pair = (team.username.as_str(), team.password.as_str());
                if let Some(first) = credentials.insert(pair, slug.as_str()) {
                    let (user, _) = pair;
                    if first == slug {
                        errors.push(format!(
                            "site {slug:?}: login {user:?} has the same password listed twice"
                        ));
                    } else {
                        errors.push(format!(
                            "login {user:?} has the same password at sites {first:?} and \
                             {slug:?}; a login and password together must identify one team"
                        ));
                    }
                }
            }
        }

        let mut tokens: BTreeMap<&str, &str> = BTreeMap::new();
        for (slug, site) in &self.sites {
            if slug.is_empty()
                || !slug
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                errors.push(format!(
                    "site {slug:?}: slug must be non-empty and use only letters, digits, '-' and '_'"
                ));
            }
            if site.token.trim().is_empty() {
                errors.push(format!("site {slug:?}: token must not be empty"));
            } else if let Some(other) = tokens.insert(site.token.as_str(), slug.as_str()) {
                errors.push(format!(
                    "site {slug:?}: token is also used by site {other:?}"
                ));
            }
            if site.teams.is_empty() {
                errors.push(format!("site {slug:?}: has no teams"));
            }
            if let Some(hours) = site.timezone_offset_hours {
                errors.push(format!(
                    "site {slug:?}: timezone_offset_hours has been replaced by timezone: \
                     write `timezone: UTC{}{:02}:00` instead",
                    if hours < 0 { '-' } else { '+' },
                    hours.abs()
                ));
            }

            // A login may appear more than once in a site, which is how a
            // team gets one password for the practice session and another for
            // the contest. The entries describe the same team, so they have
            // to agree on its name and differ in their passwords.
            let mut by_login: BTreeMap<&str, Vec<&TeamConfig>> = BTreeMap::new();
            for team in &site.teams {
                let user = team.username.as_str();
                if user.trim().is_empty() {
                    errors.push(format!("site {slug:?}: a team has an empty username"));
                    continue;
                }
                // '|' is the session payload separator.
                if user.contains('|') {
                    errors.push(format!(
                        "site {slug:?}: username {user:?} must not contain '|'"
                    ));
                }
                if team.password.is_empty() {
                    errors.push(format!(
                        "site {slug:?}: team {user:?} has an empty password"
                    ));
                }
                if team.name.trim().is_empty() {
                    errors.push(format!("site {slug:?}: team {user:?} has an empty name"));
                }
                by_login.entry(user).or_default().push(team);
            }

            for (user, entries) in &by_login {
                if entries.len() < 2 {
                    continue;
                }
                let name = entries[0].name.as_str();
                if let Some(other) = entries.iter().find(|t| t.name != name) {
                    errors.push(format!(
                        "site {slug:?}: login {user:?} is listed as both {name:?} and {:?}; \
                         a login is one team, so its entries must share a name",
                        other.name
                    ));
                }
            }
        }

        if !errors.is_empty() {
            bail!("invalid configuration:\n  - {}", errors.join("\n  - "));
        }
        Ok(())
    }

    /// Validates credentials. The pair identifies the team on its own, so no
    /// site has to be supplied.
    pub fn check_login(&self, username: &str, password: &str) -> LoginOutcome {
        let mut found: Option<&String> = None;
        for (slug, site) in &self.sites {
            for team in &site.teams {
                if team.username != username {
                    continue;
                }
                let ok: bool = team.password.as_bytes().ct_eq(password.as_bytes()).into();
                if ok {
                    found = Some(slug);
                }
            }
        }

        match found {
            Some(slug) => LoginOutcome::Ok {
                site: slug.clone(),
                username: username.to_string(),
            },
            None => LoginOutcome::Invalid,
        }
    }

    /// Where the queue is kept on disk: the configured path, or
    /// `webprint-state.jsonl` beside `config_path`.
    pub fn state_path(&self, config_path: &Path) -> PathBuf {
        self.state_file.clone().unwrap_or_else(|| {
            config_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .join("webprint-state.jsonl")
        })
    }

    /// The configured path prefix, normalized: empty when served at the root,
    /// otherwise a leading slash and no trailing one (`/print`).
    pub fn base_path(&self) -> String {
        normalize_base_path(self.base_path.as_deref().unwrap_or(""))
    }

    pub fn site(&self, slug: &str) -> Option<&SiteConfig> {
        self.sites.get(slug)
    }

    /// The zone to use for a site: its own when set, otherwise the
    /// server-wide one, otherwise UTC.
    pub fn timezone_for(&self, slug: &str) -> TimeZone {
        self.site(slug)
            .and_then(|s| s.timezone)
            .or(self.timezone)
            .unwrap_or_default()
    }

    pub fn team(&self, slug: &str, username: &str) -> Option<&TeamConfig> {
        self.site(slug)?
            .teams
            .iter()
            .find(|t| t.username == username)
    }

    /// Finds the site owning `token`, comparing in constant time.
    pub fn site_by_token(&self, token: &str) -> Option<(&str, &SiteConfig)> {
        let mut found = None;
        for (slug, site) in &self.sites {
            if site.token.as_bytes().ct_eq(token.as_bytes()).into() {
                found = Some((slug.as_str(), site));
            }
        }
        found
    }

    /// Verifies the admin token, in constant time. Always false when unset.
    pub fn check_admin_token(&self, token: &str) -> bool {
        match &self.admin_token {
            Some(expected) if !expected.is_empty() => {
                expected.as_bytes().ct_eq(token.as_bytes()).into()
            }
            _ => false,
        }
    }
}

impl SiteConfig {
    pub fn display_name(&self, slug: &str) -> String {
        self.name.clone().unwrap_or_else(|| slug.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
listen: "127.0.0.1:9000"
admin_token: "adm"
sites:
  main-hall:
    name: "Main Hall"
    token: "tok-main"
    teams:
      - { username: t1, password: p1, name: "Team One" }
      - { username: t2, password: p2, name: "Team Two" }
  annex:
    token: "tok-annex"
    teams:
      - { username: t1, password: other, name: "Annex One" }
      - { username: a2, password: p2, name: "Annex Two" }
  overflow:
    timezone: UTC-05:00
    token: "tok-overflow"
    teams:
      - { username: dup, password: same, name: "Overflow Dup" }
"#;

    /// Two sites issuing the very same credentials, which is what an operator
    /// gets by reusing one generated team list everywhere.
    const COLLIDING: &str = r#"
sites:
  a:
    token: "tok-a"
    teams:
      - { username: dup, password: same, name: "A Dup" }
  b:
    token: "tok-b"
    teams:
      - { username: dup, password: same, name: "B Dup" }
"#;

    #[test]
    fn parses_the_documented_shape_with_defaults() {
        let c = Config::parse_str(SAMPLE).unwrap();
        assert_eq!(c.listen, "127.0.0.1:9000");
        assert_eq!(c.sites.len(), 3);
        assert_eq!(c.session_hours, 24);
        assert_eq!(c.pdf.font_size, 10.0);
        assert_eq!(c.pdf.line_spacing, 10.0 / 8.0);
        // Line numbers and highlighting are on unless turned off.
        assert!(c.pdf.line_numbers);
        assert!(c.pdf.highlight);
        assert!(c.pdf.syntax_dir.is_none());
        assert_eq!(
            c.site("main-hall").unwrap().display_name("main-hall"),
            "Main Hall"
        );
        assert_eq!(c.site("annex").unwrap().display_name("annex"), "annex");
    }

    #[test]
    fn the_pdf_defaults_match_the_renderer() {
        // Two sets of defaults for the same setting would mean a minimal
        // configuration printing differently from the documented one.
        let c = Config::parse_str(SAMPLE).unwrap();
        let style = webprint_pdf::Style::default();
        assert_eq!(c.pdf.font_size, style.font_size);
        assert_eq!(c.pdf.line_spacing, style.line_spacing);
        assert_eq!(c.pdf.tab_width, style.tab_width);
        assert_eq!(c.pdf.line_numbers, style.line_numbers);
        assert_eq!(c.pdf.highlight, style.highlight);
        assert_eq!(c.pdf.max_pages, style.max_pages);
    }

    #[test]
    fn pdf_toggles_can_be_turned_off() {
        let c = Config::parse_str(
            "pdf:\n  line_numbers: false\n  highlight: false\n  syntax_dir: /srv/syntaxes\n\
             sites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
        )
        .unwrap();
        assert!(!c.pdf.line_numbers);
        assert!(!c.pdf.highlight);
        assert_eq!(
            c.pdf.syntax_dir.as_deref(),
            Some(std::path::Path::new("/srv/syntaxes"))
        );
    }

    #[test]
    fn a_site_may_override_the_server_time_zone() {
        let c = Config::parse_str(&format!("timezone: UTC+03:00\n{SAMPLE}")).unwrap();
        // The site that sets one wins; the others inherit.
        assert_eq!(c.timezone_for("overflow").to_string(), "UTC-05:00");
        assert_eq!(c.timezone_for("main-hall").to_string(), "UTC+03:00");
        assert_eq!(c.timezone_for("annex").to_string(), "UTC+03:00");
        // An unknown site falls back rather than panicking.
        assert_eq!(c.timezone_for("nosuch").to_string(), "UTC+03:00");

        // Without a server-wide setting the default is UTC.
        let c = Config::parse_str(SAMPLE).unwrap();
        assert_eq!(c.timezone_for("main-hall"), TimeZone::UTC);
        assert_eq!(c.timezone_for("overflow").offset().whole_minutes(), -300);
    }

    #[test]
    fn half_hour_zones_round_trip_through_the_config() {
        let c = Config::parse_str(
            "timezone: \"UTC+05:30\"\nsites:\n  s:\n    token: t\n    teams:\n      \
             - {username: u, password: p, name: N}\n",
        )
        .unwrap();
        assert_eq!(c.timezone_for("s").offset().whole_minutes(), 330);
        assert_eq!(c.timezone_for("s").to_string(), "UTC+05:30");
    }

    #[test]
    fn a_bad_time_zone_names_the_format() {
        let err = Config::parse_str(
            "timezone: Europe/Moscow\nsites:\n  s:\n    token: t\n    teams:\n      \
             - {username: u, password: p, name: N}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("UTC+06:00"), "{err}");
    }

    #[test]
    fn the_old_offset_setting_explains_how_to_migrate() {
        // It said nothing about what the offset was from, so it is gone.
        for yaml in [
            "timezone_offset_hours: 3\nsites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
            "sites:\n  s:\n    timezone_offset_hours: -5\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
        ] {
            let err = Config::parse_str(yaml).unwrap_err().to_string();
            assert!(err.contains("replaced by timezone"), "{err}");
            assert!(err.contains("UTC"), "{err}");
        }
    }

    #[test]
    fn base_paths_are_normalized() {
        assert_eq!(normalize_base_path(""), "");
        assert_eq!(normalize_base_path("/"), "");
        assert_eq!(normalize_base_path("   "), "");
        assert_eq!(normalize_base_path("print"), "/print");
        assert_eq!(normalize_base_path("/print"), "/print");
        assert_eq!(normalize_base_path("/print/"), "/print");
        assert_eq!(normalize_base_path("  /print/  "), "/print");
        assert_eq!(normalize_base_path("/a/b/"), "/a/b");
    }

    #[test]
    fn a_state_file_is_optional() {
        let c = Config::parse_str(SAMPLE).unwrap();
        assert!(c.state_file.is_none());
        let c = Config::parse_str(&format!(
            "state_file: /var/lib/webprint/state.jsonl\n{SAMPLE}"
        ))
        .unwrap();
        assert_eq!(
            c.state_file.as_deref(),
            Some(std::path::Path::new("/var/lib/webprint/state.jsonl"))
        );
    }

    #[test]
    fn a_base_path_can_be_configured() {
        let c = Config::parse_str(&format!("base_path: /print/\n{SAMPLE}")).unwrap();
        assert_eq!(c.base_path(), "/print");
        // Unset means served at the root.
        assert_eq!(Config::parse_str(SAMPLE).unwrap().base_path(), "");
    }

    #[test]
    fn a_nonsense_base_path_is_rejected() {
        let err = Config::parse_str(
            "base_path: \"/pr int?x\"\nsites:\n  s:\n    token: t\n    teams:\n      \
             - {username: u, password: p, name: N}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("base_path"), "{err}");
    }

    #[test]
    fn minimal_config_only_needs_sites() {
        let c = Config::parse_str(
            "sites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
        )
        .unwrap();
        assert_eq!(c.listen, "0.0.0.0:8080");
        assert_eq!(c.limits.lease_seconds, 120);
    }

    #[test]
    fn unknown_fields_are_rejected_so_typos_surface() {
        let err = Config::parse_str("sitez: {}\n").unwrap_err().to_string();
        assert!(
            err.contains("sitez") || err.contains("unknown field"),
            "{err}"
        );
    }

    #[test]
    fn a_login_and_password_together_identify_the_team() {
        let c = Config::parse_str(SAMPLE).unwrap();
        // t2 exists only at main-hall.
        assert_eq!(
            c.check_login("t2", "p2"),
            LoginOutcome::Ok {
                site: "main-hall".into(),
                username: "t2".into()
            }
        );
        // t1 exists at two sites; the password says which team it is.
        assert_eq!(
            c.check_login("t1", "p1"),
            LoginOutcome::Ok {
                site: "main-hall".into(),
                username: "t1".into()
            }
        );
        assert_eq!(
            c.check_login("t1", "other"),
            LoginOutcome::Ok {
                site: "annex".into(),
                username: "t1".into()
            }
        );
        // Everything else is simply wrong.
        assert_eq!(c.check_login("nobody", "x"), LoginOutcome::Invalid);
        assert_eq!(c.check_login("t2", "wrong"), LoginOutcome::Invalid);
        assert_eq!(c.check_login("t1", "nonsense"), LoginOutcome::Invalid);
        assert_eq!(c.check_login("", ""), LoginOutcome::Invalid);
    }

    #[test]
    fn one_login_may_have_a_practice_and_a_contest_password() {
        let c = Config::parse_str(
            "sites:\n  main:\n    token: t\n    teams:\n      \
             - {username: team01, password: practice-pw, name: Hello World}\n      \
             - {username: team01, password: contest-pw, name: Hello World}\n",
        )
        .expect("two passwords for one login is a valid configuration");

        // Either password signs the same team in.
        for password in ["practice-pw", "contest-pw"] {
            assert_eq!(
                c.check_login("team01", password),
                LoginOutcome::Ok {
                    site: "main".into(),
                    username: "team01".into()
                },
                "{password}"
            );
        }
        // A retired or mistyped password still does not, and the team is
        // told it is wrong rather than being sent to a site link.
        assert_eq!(c.check_login("team01", "old-pw"), LoginOutcome::Invalid);

        // And the team is one team, with one name.
        assert_eq!(c.team("main", "team01").unwrap().name, "Hello World");
        assert_eq!(c.sites["main"].teams.len(), 2);
    }

    #[test]
    fn the_same_credentials_twice_stop_the_server() {
        // Reusing one generated team list at two venues would make a sign-in
        // impossible to resolve, so it is refused before the contest starts
        // rather than discovered during it.
        let err = Config::parse_str(COLLIDING).unwrap_err().to_string();
        assert!(err.contains("the same password at sites"), "{err}");
        assert!(err.contains("\"a\"") && err.contains("\"b\""), "{err}");
        assert!(err.contains("must identify one team"), "{err}");

        // The same thing within one site is caught too.
        let err = Config::parse_str(
            "sites:\n  s:\n    token: t\n    teams:\n      \
             - {username: u, password: p, name: N}\n      \
             - {username: u, password: p, name: N}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("same password listed twice"), "{err}");

        // Differing passwords are fine, within a site or across them.
        Config::parse_str(
            "sites:\n  a:\n    token: t1\n    teams:\n      \
             - {username: u, password: p, name: A}\n  b:\n    token: t2\n    teams:\n      \
             - {username: u, password: q, name: B}\n",
        )
        .expect("one login at two sites with different passwords is fine");
    }

    #[test]
    fn tokens_map_back_to_their_site() {
        let c = Config::parse_str(SAMPLE).unwrap();
        assert_eq!(c.site_by_token("tok-annex").unwrap().0, "annex");
        assert!(c.site_by_token("nope").is_none());
        assert!(c.site_by_token("").is_none());
    }

    #[test]
    fn admin_token_is_disabled_when_unset() {
        let c = Config::parse_str(
            "sites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
        )
        .unwrap();
        assert!(!c.check_admin_token(""));
        assert!(!c.check_admin_token("anything"));
        let c = Config::parse_str(SAMPLE).unwrap();
        assert!(c.check_admin_token("adm"));
        assert!(!c.check_admin_token("adm2"));
    }

    #[test]
    fn validation_catches_operator_mistakes() {
        let cases = [
            ("sites: {}\n", "no sites"),
            (
                "sites:\n  s:\n    token: \"\"\n    teams:\n      - {username: u, password: p, name: N}\n",
                "token must not be empty",
            ),
            ("sites:\n  s:\n    token: t\n    teams: []\n", "has no teams"),
            (
                "sites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n      - {username: u, password: p, name: N}\n",
                "same password listed twice",
            ),
            (
                "sites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n      - {username: u, password: q, name: M}\n",
                "must share a name",
            ),
            (
                "sites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: \"\", name: N}\n",
                "empty password",
            ),
            (
                "sites:\n  s:\n    token: t\n    teams:\n      - {username: \"a|b\", password: p, name: N}\n",
                "must not contain",
            ),
            (
                "sites:\n  a:\n    token: same\n    teams:\n      - {username: u, password: p, name: N}\n  b:\n    token: same\n    teams:\n      - {username: v, password: p, name: M}\n",
                "also used by site",
            ),
            (
                "sites:\n  \"bad slug\":\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
                "slug must be",
            ),
            (
                "session_hours: 0\nsites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
                "session_hours",
            ),
            (
                "pdf:\n  max_pages: 0\nsites:\n  s:\n    token: t\n    teams:\n      - {username: u, password: p, name: N}\n",
                "max_pages",
            ),
        ];
        for (yaml, expected) in cases {
            let err = Config::parse_str(yaml).unwrap_err().to_string();
            assert!(err.contains(expected), "for {yaml:?}\n  got: {err}");
        }
    }
}
