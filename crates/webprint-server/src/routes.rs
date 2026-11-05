//! HTTP routes: the team-facing web UI, the print-client API and stats.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use webprint_pdf::PrintJob;

use crate::auth::{clear_cookie, session_cookie};
use crate::config::LoginOutcome;
use crate::queue::{CompleteError, JobId, JobMeta, JobView, SiteStats, SubmitError};
use crate::state::{api_error, AdminAuth, AppState, SiteAuth, Team};
use crate::util::now_unix;
use crate::views;

/// Builds the router.
///
/// With a `base_path` configured, every route is registered twice: once bare
/// and once under the prefix. A proxy that strips the prefix
/// (`proxy_pass http://127.0.0.1:12345/;`) reaches the first, one that passes
/// it through reaches the second, and neither needs special configuration.
pub fn router(state: AppState) -> Router {
    let base = state.base_path.to_string();
    let mut app = routes("");
    if !base.is_empty() {
        app = app.merge(routes(&base));
    }
    app.with_state(state)
}

/// The route table, optionally under a path prefix.
fn routes(prefix: &str) -> Router<AppState> {
    let at = |suffix: &str| format!("{prefix}{suffix}");
    Router::new()
        .route(&at("/"), get(index))
        .route(&at("/login"), get(login_page).post(login_submit))
        .route(&at("/logout"), post(logout))
        .route(&at("/print"), get(print_page).post(print_submit))
        .route(&at("/api/print"), post(api_print))
        .route(&at("/api/my/jobs"), get(api_my_jobs))
        .route(&at("/stats"), get(stats_page))
        .route(&at("/api/v1/ping"), get(api_ping))
        .route(&at("/api/v1/claim"), post(api_claim))
        .route(&at("/api/v1/jobs/{id}/pdf"), get(api_job_pdf))
        .route(&at("/api/v1/jobs/{id}/complete"), post(api_complete))
        .route(&at("/api/v1/stats"), get(api_site_stats))
        .route(&at("/api/v1/admin/stats"), get(api_admin_stats))
        .route(&at("/healthz"), get(healthz))
}

// ---------------------------------------------------------------- web UI

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if signed_in(&state, &headers) {
        Redirect::to(&state.path("/print")).into_response()
    } else {
        Redirect::to(&state.path("/login")).into_response()
    }
}

fn signed_in(state: &AppState, headers: &HeaderMap) -> bool {
    let mut parts = axum::http::Request::new(());
    *parts.headers_mut() = headers.clone();
    let (parts, _) = parts.into_parts();
    Team::from_parts(state, &parts).is_some()
}

async fn login_page(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if signed_in(&state, &headers) {
        return Redirect::to(&state.path("/print")).into_response();
    }
    Html(views::login(
        &state.base_path,
        q.get("error").map(String::as_str),
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn login_submit(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    let username = form.username.trim();

    match state.config.check_login(username, &form.password) {
        LoginOutcome::Ok { site, username } => {
            let expires_at = now_unix() + state.config.session_hours * 3_600;
            let cookie = state.key.sign(&site, &username, expires_at);
            tracing::info!(site = %site, team = %username, "sign in");
            (
                StatusCode::SEE_OTHER,
                [
                    (
                        header::SET_COOKIE,
                        session_cookie(
                            &cookie,
                            state.config.session_hours * 3_600,
                            state.config.secure_cookies,
                            &state.base_path,
                        ),
                    ),
                    (header::LOCATION, state.path("/print")),
                ],
            )
                .into_response()
        }
        LoginOutcome::Invalid => {
            tracing::warn!(team = %username, "failed sign in");
            (
                StatusCode::UNAUTHORIZED,
                Html(views::login(
                    &state.base_path,
                    Some("Wrong login or password."),
                )),
            )
                .into_response()
        }
    }
}

async fn logout(State(state): State<AppState>) -> Response {
    (
        StatusCode::SEE_OTHER,
        [
            (
                header::SET_COOKIE,
                clear_cookie(state.config.secure_cookies, &state.base_path),
            ),
            (header::LOCATION, state.path("/login")),
        ],
    )
        .into_response()
}

async fn print_page(
    State(state): State<AppState>,
    team: Team,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    // Flash message from the no-JavaScript form round trip.
    let notice = match (q.get("ok"), q.get("pages"), q.get("error")) {
        (Some(id), pages, _) => {
            let pages = pages.and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
            Some((
                true,
                format!(
                    "Job {id} queued - {pages} {}. Collect it from the printer.",
                    if pages == 1 { "page" } else { "pages" }
                ),
            ))
        }
        (None, _, Some(err)) => Some((false, err.clone())),
        _ => None,
    };

    let jobs = state.store.team_jobs(&team.site, &team.username, 10);
    let tz = state.config.timezone_for(&team.site);
    Html(views::print_page(
        &state.base_path,
        &team.name,
        &team.site_name,
        &jobs,
        tz,
        notice.as_ref().map(|(ok, msg)| (*ok, msg.as_str())),
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct PrintForm {
    content: String,
}

/// Plain form submit: used when JavaScript is unavailable.
async fn print_submit(
    State(state): State<AppState>,
    team: Team,
    Form(form): Form<PrintForm>,
) -> Response {
    match submit_job(&state, &team, &form.content).await {
        Ok(job) => Redirect::to(&state.path(&format!("/print?ok={}&pages={}", job.id, job.pages)))
            .into_response(),
        Err((_, message)) => {
            Redirect::to(&state.path(&format!("/print?error={}", urlencode(&message))))
                .into_response()
        }
    }
}

#[derive(Debug, Serialize)]
struct PrintResponse {
    id: JobId,
    pages: usize,
    lines: usize,
}

/// JSON submit, used by the print page's JavaScript.
async fn api_print(
    State(state): State<AppState>,
    team: Team,
    Json(req): Json<PrintForm>,
) -> Response {
    match submit_job(&state, &team, &req.content).await {
        Ok(job) => (
            StatusCode::OK,
            Json(PrintResponse {
                id: job.id,
                pages: job.pages,
                lines: job.lines,
            }),
        )
            .into_response(),
        Err((status, message)) => api_error(status, &message),
    }
}

#[derive(Debug, Serialize)]
struct MyJobsResponse {
    /// Pre-rendered table, so the page does not duplicate the markup.
    html: String,
    jobs: Vec<JobView>,
}

async fn api_my_jobs(State(state): State<AppState>, team: Team) -> Response {
    let jobs = state.store.team_jobs(&team.site, &team.username, 10);
    Json(MyJobsResponse {
        html: views::jobs_table(&jobs, state.config.timezone_for(&team.site)),
        jobs,
    })
    .into_response()
}

/// Validates, renders and enqueues one job.
async fn submit_job(
    state: &AppState,
    team: &Team,
    content: &str,
) -> Result<JobView, (StatusCode, String)> {
    if content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Nothing to print.".into()));
    }
    let chars = content.chars().count();

    // Check the quota before spending CPU on a render.
    state
        .store
        .check_capacity(&team.site, &team.username)
        .map_err(submit_error)?;

    let now = now_unix();
    let id = state.store.reserve_id();
    let content: Arc<str> = Arc::from(content);

    let renderer = state.renderer.clone();
    let measured = {
        let (team_name, site, content) = (team.name.clone(), team.site.clone(), content.clone());
        tokio::task::spawn_blocking(move || {
            renderer.measure(&PrintJob {
                team: &team_name,
                site: &site,
                label: &id.to_string(),
                submitted_at: "",
                content: &content,
                language: None,
            })
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "layout task failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not lay the document out.".to_string(),
            )
        })?
    };

    if measured.lines > state.config.limits.max_lines {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "The job is too long.".to_string(),
        ));
    }

    let meta = JobMeta {
        pages: measured.pages,
        lines: measured.lines,
        chars,
    };
    let job = state
        .store
        .submit(
            &team.site,
            id,
            &team.username,
            &team.name,
            content,
            meta,
            now,
        )
        .map_err(submit_error)?;

    tracing::info!(
        site = %team.site,
        team = %team.username,
        job = job.id,
        pages = job.pages,
        "job queued"
    );
    Ok(job)
}

fn submit_error(err: SubmitError) -> (StatusCode, String) {
    match err {
        SubmitError::UnknownSite => (
            StatusCode::NOT_FOUND,
            "Your site no longer exists; sign in again.".to_string(),
        ),
        SubmitError::TooManyQueued { .. } => (StatusCode::TOO_MANY_REQUESTS, err.to_string()),
    }
}

/// Flattens a client-reported error into one short, readable line.
fn tidy_error(raw: &str) -> String {
    let flattened = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "the print client reported a failure".to_string();
    }
    let mut out: String = collapsed.chars().take(200).collect();
    if collapsed.chars().count() > 200 {
        out.push('…');
    }
    out
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

// ------------------------------------------------------- print client API

#[derive(Debug, Deserialize, Default)]
struct ClaimRequest {
    /// Identifier the client reports, shown in the stats.
    #[serde(default)]
    client: Option<String>,
    /// How long the server may hold the request open waiting for a job.
    #[serde(default)]
    wait_ms: Option<u64>,
}

async fn api_ping(
    State(state): State<AppState>,
    site: SiteAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    // A ping carries the client's name so it appears in the stats straight
    // away, rather than only once it has taken its first job.
    if let Some(client) = q.get("client").filter(|c| !c.is_empty()) {
        state.store.touch_client(&site.site, client, now_unix());
    }
    Json(serde_json::json!({
        "ok": true,
        "site": site.site,
        "name": site.site_name,
        "server_time": now_unix(),
    }))
    .into_response()
}

/// Long-polls for the next job. Returns 204 when nothing arrives in time.
async fn api_claim(
    State(state): State<AppState>,
    site: SiteAuth,
    body: Option<Json<ClaimRequest>>,
) -> Response {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let client = req.client.unwrap_or_else(|| "unnamed".to_string());
    let limits = state.store.limits();
    let wait = Duration::from_millis(
        req.wait_ms
            .unwrap_or(limits.poll_wait_seconds * 1_000)
            .min(limits.poll_wait_seconds * 1_000),
    );

    let mut shutdown = state.shutdown.clone();
    match state
        .store
        .claim_wait(&site.site, &client, wait, &mut shutdown)
        .await
    {
        Some(job) => {
            tracing::info!(site = %site.site, job = job.id, %client, "job claimed");
            Json(serde_json::json!({ "job": job })).into_response()
        }
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn api_job_pdf(
    State(state): State<AppState>,
    site: SiteAuth,
    Path(id): Path<JobId>,
) -> Response {
    let Some(source) = state.store.source(&site.site, id) else {
        return api_error(
            StatusCode::NOT_FOUND,
            "no document for this job: it is not in flight any more",
        );
    };

    let renderer = state.renderer.clone();
    let submitted_at =
        crate::util::format_ts(source.created_at, state.config.timezone_for(&site.site));
    let slug = site.site.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        renderer.render(&PrintJob {
            team: &source.team,
            site: &slug,
            label: &id.to_string(),
            submitted_at: &submitted_at,
            content: &source.content,
            language: None,
        })
    })
    .await;

    match rendered {
        Ok(rendered) => {
            tracing::info!(
                site = %site.site,
                job = id,
                pages = rendered.pages,
                bytes = rendered.bytes.len(),
                language = ?rendered.language,
                "document rendered"
            );
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/pdf".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("inline; filename=\"webprint-{id}.pdf\""),
                    ),
                ],
                rendered.bytes,
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, job = id, "render task failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not render the document",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct CompleteRequest {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

async fn api_complete(
    State(state): State<AppState>,
    site: SiteAuth,
    Path(id): Path<JobId>,
    Json(req): Json<CompleteRequest>,
) -> Response {
    let error = req.error.map(|e| tidy_error(&e));

    match state
        .store
        .complete(&site.site, id, req.ok, error, now_unix())
    {
        Ok(job) => {
            if req.ok {
                tracing::info!(site = %site.site, job = id, pages = job.pages, "job printed");
            } else {
                tracing::warn!(site = %site.site, job = id, error = ?job.error, "job failed");
            }
            Json(serde_json::json!({ "job": job })).into_response()
        }
        Err(err) => {
            let status = match err {
                CompleteError::UnknownSite | CompleteError::UnknownJob => StatusCode::NOT_FOUND,
                CompleteError::NotClaimed(_) => StatusCode::CONFLICT,
            };
            api_error(status, &err.to_string())
        }
    }
}

/// A site's counters plus the zone its times should be shown in, so a client
/// does not have to be told the zone separately.
#[derive(Debug, Serialize)]
struct SiteStatsView {
    #[serde(flatten)]
    stats: SiteStats,
    /// Canonical UTC offset, e.g. `UTC+06:00`.
    timezone: String,
}

#[derive(Debug, Serialize)]
struct StatsResponse {
    #[serde(flatten)]
    site: SiteStatsView,
    /// Most recent jobs, newest first.
    recent: Vec<JobView>,
    server_time: i64,
}

async fn api_site_stats(State(state): State<AppState>, site: SiteAuth) -> Response {
    let now = now_unix();
    match state.store.stats(&site.site, now) {
        Some(stats) => Json(StatsResponse {
            site: SiteStatsView {
                timezone: state.config.timezone_for(&site.site).to_string(),
                stats,
            },
            recent: state.store.recent_jobs(&site.site, 20),
            server_time: now,
        })
        .into_response(),
        None => api_error(StatusCode::NOT_FOUND, "unknown site"),
    }
}

// ------------------------------------------------------------------ admin

async fn api_admin_stats(State(state): State<AppState>, _: AdminAuth) -> Response {
    let now = now_unix();
    let sites: Vec<SiteStatsView> = state
        .store
        .all_stats(now)
        .into_iter()
        .map(|stats| SiteStatsView {
            timezone: state.config.timezone_for(&stats.site).to_string(),
            stats,
        })
        .collect();
    Json(serde_json::json!({ "sites": sites, "server_time": now })).into_response()
}

async fn stats_page(State(state): State<AppState>, _: AdminAuth) -> Response {
    let now = now_unix();
    // Each site's times are shown in that site's own zone.
    let sites: Vec<_> = state
        .store
        .all_stats(now)
        .into_iter()
        .map(|stats| {
            let tz = state.config.timezone_for(&stats.site);
            (stats, tz)
        })
        .collect();
    Html(views::stats_page(
        &sites,
        now,
        state.config.timezone.unwrap_or_default(),
    ))
    .into_response()
}

async fn healthz() -> &'static str {
    "ok\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_errors_become_one_readable_line() {
        assert_eq!(
            tidy_error("print command failed (exit 1): 'x' is not recognized\r\noperable program."),
            "print command failed (exit 1): 'x' is not recognized operable program."
        );
        assert_eq!(
            tidy_error("  lp: not   responding \n"),
            "lp: not responding"
        );
        assert_eq!(tidy_error(""), "the print client reported a failure");
        assert_eq!(
            tidy_error("   \n\t "),
            "the print client reported a failure"
        );

        let long = tidy_error(&"e".repeat(500));
        assert_eq!(long.chars().count(), 201);
        assert!(long.ends_with('…'));

        // Non-ASCII survives, and truncation respects char boundaries.
        assert_eq!(tidy_error("принтер\nне отвечает"), "принтер не отвечает");
        assert!(tidy_error(&"ж".repeat(300)).chars().count() == 201);
    }

    #[test]
    fn urlencoding_escapes_what_it_must() {
        assert_eq!(urlencode("a b"), "a+b");
        assert_eq!(urlencode("ok-1_2.3~"), "ok-1_2.3~");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("Ж"), "%D0%96");
    }
}
