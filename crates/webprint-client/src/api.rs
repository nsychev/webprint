//! HTTP client for the webprint server API.

use std::time::Duration;

use serde::Deserialize;

/// A job handed to this client.
#[derive(Clone, Debug, Deserialize)]
pub struct ClaimedJob {
    pub id: u64,
    pub team: String,
    pub username: String,
    pub site: String,
    pub pages: usize,
    pub lines: usize,
    pub created_at: i64,
    pub attempt: u32,
    pub pdf_url: String,
}

#[derive(Debug, Deserialize)]
struct ClaimResponse {
    job: ClaimedJob,
}

/// Server identity, returned by `ping`.
#[derive(Clone, Debug, Deserialize)]
pub struct Ping {
    pub site: String,
    pub name: String,
}

/// One job in the server's recent history.
#[derive(Clone, Debug, Deserialize)]
pub struct JobView {
    pub id: u64,
    pub state: String,
    pub team: String,
    pub username: String,
    pub pages: usize,
    pub created_at: i64,
    #[serde(default)]
    pub error: Option<String>,
}

/// Queue counters for the site this client serves.
#[derive(Clone, Debug, Deserialize)]
pub struct Stats {
    pub site: String,
    pub name: String,
    pub queued: usize,
    pub printing: usize,
    pub processed: u64,
    pub failed: u64,
    pub submitted: u64,
    pub pages_printed: u64,
    /// The zone this site's times should be shown in, e.g. `UTC+06:00`.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Every print client polling this site. More than one means they share
    /// the queue.
    #[serde(default)]
    pub clients: Vec<ClientInfo>,
    #[serde(default)]
    pub recent: Vec<JobView>,
}

/// Another print client working the same site.
#[derive(Clone, Debug, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub last_seen: i64,
    pub claimed: u64,
}

/// What went wrong talking to the server.
#[derive(Debug)]
pub enum ApiError {
    /// The site token was rejected; retrying will not help.
    Auth(String),
    /// The server could not be reached.
    Transport(String),
    /// The server answered, but not as expected.
    Server(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth(m) | ApiError::Transport(m) | ApiError::Server(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    pub fn is_auth(&self) -> bool {
        matches!(self, ApiError::Auth(_))
    }
}

type Result<T> = std::result::Result<T, ApiError>;

pub struct Api {
    client: reqwest::blocking::Client,
    base: String,
    token: String,
    name: String,
}

/// Installs the TLS backend for `reqwest`.
pub fn install_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Only fails if something already installed a provider, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Api {
    pub fn new(base: &str, token: &str, name: &str, poll_wait: Duration) -> Result<Self> {
        install_tls_provider();
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // Increase the read timeout for the long-polling to work.
            .timeout(poll_wait + Duration::from_secs(20))
            .user_agent(concat!("webprint-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ApiError::Transport(format!("could not build the HTTP client: {e}")))?;

        Ok(Self {
            client,
            base: normalize_base(base),
            token: token.to_string(),
            name: name.to_string(),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Checks connectivity and the token, and learns the site name.
    pub fn ping(&self) -> Result<Ping> {
        let res = self
            .client
            .get(self.url("/api/v1/ping"))
            .query(&[("client", self.name.as_str())])
            .bearer_auth(&self.token)
            .send()
            .map_err(transport)?;
        json(check(res)?)
    }

    /// Long-polls for the next job. `Ok(None)` means the queue stayed empty.
    pub fn claim(&self, wait: Duration) -> Result<Option<ClaimedJob>> {
        let res = self
            .client
            .post(self.url("/api/v1/claim"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "client": self.name,
                "wait_ms": wait.as_millis() as u64,
            }))
            .send()
            .map_err(transport)?;

        if res.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let body: ClaimResponse = json(check(res)?)?;
        Ok(Some(body.job))
    }

    /// Downloads a claimed job's PDF.
    pub fn fetch_pdf(&self, job: &ClaimedJob) -> Result<Vec<u8>> {
        let res = self
            .client
            .get(self.url(&job.pdf_url))
            .bearer_auth(&self.token)
            .send()
            .map_err(transport)?;
        let bytes = check(res)?.bytes().map_err(transport)?;

        if !bytes.starts_with(b"%PDF") {
            return Err(ApiError::Server(format!(
                "job {} did not come back as a PDF ({} bytes)",
                job.id,
                bytes.len()
            )));
        }
        Ok(bytes.to_vec())
    }

    /// Reports the outcome of a print attempt.
    pub fn complete(&self, id: u64, ok: bool, error: Option<&str>) -> Result<()> {
        let res = self
            .client
            .post(self.url(&format!("/api/v1/jobs/{id}/complete")))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "ok": ok, "error": error }))
            .send()
            .map_err(transport)?;
        check(res)?;
        Ok(())
    }

    pub fn stats(&self) -> Result<Stats> {
        let res = self
            .client
            .get(self.url("/api/v1/stats"))
            .bearer_auth(&self.token)
            .send()
            .map_err(transport)?;
        json(check(res)?)
    }
}

/// Normalizes a base URL: adds http:// scheme if not set, drops a trailing slash.
pub fn normalize_base(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.starts_with("http://") || base.starts_with("https://") {
        base.to_string()
    } else {
        format!("http://{base}")
    }
}

fn check(res: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    // The server sends {"error": "..."} for every failure it can explain.
    let detail = res
        .json::<serde_json::Value>()
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| status.to_string());

    if status == reqwest::StatusCode::UNAUTHORIZED {
        Err(ApiError::Auth(detail))
    } else {
        Err(ApiError::Server(format!("{status}: {detail}")))
    }
}

fn json<T: serde::de::DeserializeOwned>(res: reqwest::blocking::Response) -> Result<T> {
    res.json()
        .map_err(|e| ApiError::Server(format!("unexpected response from the server: {e}")))
}

/// Converts a transport error into an `ApiError`.
fn transport(err: reqwest::Error) -> ApiError {
    let message = if err.is_connect() {
        "cannot reach the server (connection refused or host unreachable)".to_string()
    } else if err.is_timeout() {
        "the server did not answer in time".to_string()
    } else if err.is_request() {
        format!("bad request: {err}")
    } else {
        let mut source: &dyn std::error::Error = &err;
        let mut message = err.to_string();
        while let Some(inner) = source.source() {
            message = inner.to_string();
            source = inner;
        }
        message
    };
    ApiError::Transport(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_get_a_scheme_and_lose_trailing_slashes() {
        assert_eq!(normalize_base("10.0.0.5:8080"), "http://10.0.0.5:8080");
        assert_eq!(normalize_base("http://host/"), "http://host");
        assert_eq!(normalize_base("https://host:443/"), "https://host:443");
        assert_eq!(normalize_base("  host  "), "http://host");
    }

    #[test]
    fn https_bases_are_preserved_and_the_tls_stack_loads() {
        // Building the client installs and exercises the TLS provider.
        let api = Api::new(
            "https://print.example:8443/",
            "tok",
            "c",
            Duration::from_secs(1),
        )
        .expect("an https client builds");
        assert_eq!(api.base(), "https://print.example:8443");
        // Installing twice must not panic.
        assert!(Api::new("https://x", "t", "c", Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn urls_are_built_under_the_base() {
        let api = Api::new("host:8080", "tok", "c", Duration::from_secs(1)).unwrap();
        assert_eq!(api.base(), "http://host:8080");
        assert_eq!(api.url("/api/v1/ping"), "http://host:8080/api/v1/ping");
    }

    #[test]
    fn auth_errors_are_distinguished_from_the_rest() {
        assert!(ApiError::Auth("x".into()).is_auth());
        assert!(!ApiError::Transport("x".into()).is_auth());
        assert!(!ApiError::Server("x".into()).is_auth());
        assert_eq!(ApiError::Transport("boom".into()).to_string(), "boom");
    }

    #[test]
    fn job_views_tolerate_a_missing_error_field() {
        let job: JobView = serde_json::from_str(
            r#"{"id":1,"state":"printed","team":"T","username":"t1","pages":2,"created_at":5}"#,
        )
        .unwrap();
        assert_eq!(job.id, 1);
        assert_eq!(job.error, None);
    }

    #[test]
    fn stats_carry_the_sites_zone_and_other_clients() {
        let stats: Stats = serde_json::from_str(
            r#"{"site":"s","name":"S","queued":0,"printing":0,"processed":0,"failed":0,
                "submitted":0,"pages_printed":0,"timezone":"UTC+06:00",
                "clients":[{"name":"printer-a","last_seen":5,"claimed":3}],"recent":[]}"#,
        )
        .unwrap();
        assert_eq!(stats.timezone.as_deref(), Some("UTC+06:00"));
        assert_eq!(stats.clients.len(), 1);
        assert_eq!(stats.clients[0].name, "printer-a");
        assert_eq!(stats.clients[0].claimed, 3);
    }

    #[test]
    fn stats_tolerate_a_missing_recent_list() {
        let stats: Stats = serde_json::from_str(
            r#"{"site":"s","name":"S","queued":1,"printing":0,"processed":2,"failed":0,
                "submitted":3,"pages_printed":4,"teams":5,"per_team":[],"server_time":9}"#,
        )
        .unwrap();
        assert_eq!(stats.queued, 1);
        assert!(stats.recent.is_empty());
    }
}
