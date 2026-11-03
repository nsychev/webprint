//! Shared application state and the authentication extractors.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::{header, request::Parts, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use webprint_pdf::Renderer;

use crate::auth::{cookie_value, SessionKey, COOKIE_NAME};
use crate::config::Config;
use crate::queue::Store;
use crate::util::now_unix;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub renderer: Arc<Renderer>,
    pub key: Arc<SessionKey>,
    /// Public path prefix, normalized: empty at the root, else `/print`.
    pub base_path: Arc<String>,
    /// Set when the server is shutting down, so requests held open for a
    /// client - the long poll - can finish promptly.
    pub shutdown: tokio::sync::watch::Receiver<bool>,
    /// Kept alongside the receiver: a watch channel whose sender has been
    /// dropped reports a change at once, which would end every long poll
    /// immediately.
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
}

impl AppState {
    /// Builds the state with a store made by `make_store`, which is how the
    /// server attaches on-disk state.
    pub fn with_store(
        config: Config,
        renderer: Renderer,
        make_store: impl FnOnce(&Config) -> Store,
    ) -> Self {
        let key = SessionKey::new(config.secret.as_deref());
        let store = make_store(&config);
        let base_path = config.base_path();
        let (shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
        Self {
            config: Arc::new(config),
            store: Arc::new(store),
            renderer: Arc::new(renderer),
            key: Arc::new(key),
            base_path: Arc::new(base_path),
            shutdown,
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }

    /// Tells requests that are held open that the server is stopping.
    pub fn begin_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Builds a public URL path: `/login` becomes `/print/login` when the
    /// server is proxied under `/print`.
    pub fn path(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.base_path)
    }
}

/// An authenticated team, resolved from the session cookie and re-checked
/// against the current configuration on every request.
#[derive(Debug, Clone)]
pub struct Team {
    pub site: String,
    pub site_name: String,
    pub username: String,
    pub name: String,
}

impl Team {
    /// Resolves a session cookie into a team, if it is still valid.
    pub fn from_parts(state: &AppState, parts: &Parts) -> Option<Self> {
        let cookies = parts
            .headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok());
        let raw = cookies
            .filter_map(|c| cookie_value(c, COOKIE_NAME))
            .next()?;
        let session = state.key.verify(&raw, now_unix())?;

        // The team must still exist in the configuration: a reload may have
        // removed it, and a stale cookie must not keep working.
        let site = state.config.site(&session.site)?;
        let team = state.config.team(&session.site, &session.username)?;
        Some(Team {
            site: session.site.clone(),
            site_name: site.display_name(&session.site),
            username: team.username.clone(),
            name: team.name.clone(),
        })
    }
}

impl FromRequestParts<AppState> for Team {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match Team::from_parts(state, parts) {
            Some(team) => Ok(team),
            None => Err(unauthenticated(parts, &state.path("/login"))),
        }
    }
}

/// Browsers get sent to the sign-in page; API callers get a 401.
fn unauthenticated(parts: &Parts, login_path: &str) -> Response {
    let wants_json = parts
        .headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/json"))
        || parts.uri.path().starts_with("/api/");
    if wants_json {
        (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"not signed in"}"#,
        )
            .into_response()
    } else {
        Redirect::to(login_path).into_response()
    }
}

/// A print client authenticated by its site token.
#[derive(Debug, Clone)]
pub struct SiteAuth {
    pub site: String,
    pub site_name: String,
}

impl FromRequestParts<AppState> for SiteAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts).ok_or_else(|| {
            api_error(
                StatusCode::UNAUTHORIZED,
                "missing site token: send 'Authorization: Bearer <site token>'",
            )
        })?;
        let (slug, site) = state
            .config
            .site_by_token(&token)
            .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "invalid site token"))?;
        Ok(SiteAuth {
            site: slug.to_string(),
            site_name: site.display_name(slug),
        })
    }
}

/// Reads a bearer token from the `Authorization` header or `?token=`.
fn bearer_token(parts: &Parts) -> Option<String> {
    if let Some(value) = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    query_param(parts, "token")
}

/// An operator authenticated by the admin token.
#[derive(Debug, Clone)]
pub struct AdminAuth;

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.config.admin_token.is_none() {
            return Err(api_error(
                StatusCode::NOT_FOUND,
                "stats are disabled: set admin_token in the server configuration",
            ));
        }
        let token = bearer_token(parts)
            .or_else(|| {
                parts
                    .headers
                    .get("x-admin-token")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if state.config.check_admin_token(&token) {
            Ok(AdminAuth)
        } else {
            Err(api_error(StatusCode::UNAUTHORIZED, "invalid admin token"))
        }
    }
}

pub fn query_param(parts: &Parts, key: &str) -> Option<String> {
    let query = parts.uri.query()?;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Minimal `application/x-www-form-urlencoded` value decoding.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A JSON error body with a status code.
pub fn api_error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_handles_plus_and_escapes() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("main%2Dhall"), "main-hall");
        assert_eq!(percent_decode("%D0%96"), "Ж");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode(""), "");
    }
}
