//! End-to-end HTTP tests driving the router directly.
//!
//! The server's modules are compiled in directly; some items are only used
//! by `main.rs`, which is not part of this binary.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

// The binary's modules, compiled into the test as a library would be.
#[path = "../src/auth.rs"]
mod auth;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/journal.rs"]
mod journal;
#[path = "../src/queue.rs"]
mod queue;
#[path = "../src/routes.rs"]
mod routes;
#[path = "../src/state.rs"]
mod state;
#[path = "../src/timezone.rs"]
mod timezone;
#[path = "../src/util.rs"]
mod util;
#[path = "../src/views.rs"]
mod views;

use config::Config;
use state::AppState;

const CONFIG: &str = r#"
admin_token: "adm-token"
secret: "test-secret"
limits:
  max_queued_per_team: 2
  poll_wait_seconds: 1
sites:
  main:
    name: "Main Hall"
    token: "tok-main"
    teams:
      - { username: t1, password: pw1, name: "Team One" }
      - { username: t2, password: pw2, name: "Team Two" }
    # One login, two passwords: practice and contest.
      - { username: t3, password: pw-practice, name: "Team Three" }
      - { username: t3, password: pw-contest, name: "Team Three" }
  annex:
    token: "tok-annex"
    timezone: UTC+05:00
    teams:
      - { username: a1, password: pwa, name: "Annex One" }
"#;

struct Harness {
    app: axum::Router,
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("not JSON ({e}): {}", self.text());
        })
    }
    fn location(&self) -> String {
        self.headers
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
    fn set_cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().to_string())
    }
}

impl Harness {
    fn new(yaml: &str) -> Self {
        let config = Config::parse_str(yaml).expect("valid test config");
        // Plain renderer: the grammars cost time and these tests do not need
        // highlighting. One test below opts in.
        let state = AppState::with_store(config, webprint_pdf::Renderer::new(), queue::Store::new);
        Self {
            app: routes::router(state),
        }
    }

    fn with_highlighting(yaml: &str) -> Self {
        let config = Config::parse_str(yaml).expect("valid test config");
        let state = AppState::with_store(
            config,
            webprint_pdf::Renderer::with_highlighting(),
            queue::Store::new,
        );
        Self {
            app: routes::router(state),
        }
    }

    async fn send(&self, req: Request<Body>) -> Res {
        let res = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("router response");
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        Res {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str) -> Res {
        self.send(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    async fn get_as(&self, uri: &str, cookie: &str) -> Res {
        self.send(
            Request::get(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post_form(&self, uri: &str, body: &str, cookie: Option<&str>) -> Res {
        let mut req =
            Request::post(uri).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    async fn post_json(&self, uri: &str, body: serde_json::Value, cookie: Option<&str>) -> Res {
        let mut req = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    async fn post_token(&self, uri: &str, token: &str, body: serde_json::Value) -> Res {
        self.send(
            Request::post(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn get_token(&self, uri: &str, token: &str) -> Res {
        self.send(
            Request::get(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// Signs in and returns the session cookie.
    async fn sign_in(&self, username: &str, password: &str) -> String {
        let res = self
            .post_form(
                "/login",
                &format!("username={username}&password={password}"),
                None,
            )
            .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.text());
        // Under a path prefix this is /print/print, at the root just /print.
        assert!(res.location().ends_with("/print"), "{}", res.location());
        let cookie = res.set_cookie().expect("session cookie");
        cookie.split(';').next().unwrap().to_string()
    }
}

#[tokio::test]
async fn health_and_root_redirects() {
    let h = Harness::new(CONFIG);
    let res = h.get("/healthz").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.text().trim(), "ok");

    let res = h.get("/").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/login");
}

#[tokio::test]
async fn the_print_page_requires_a_session() {
    let h = Harness::new(CONFIG);
    let res = h.get("/print").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/login");

    // API callers get a status code, not a redirect.
    let res = h.get("/api/my/jobs").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.json()["error"], "not signed in");
}

#[tokio::test]
async fn there_is_one_sign_in_page() {
    let h = Harness::new(CONFIG);
    let res = h.get("/login").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.text().contains("name=\"username\""));
    // No per-site sign-in page, and nothing asking which site this is.
    assert!(!res.text().contains("name=\"site\""), "{}", res.text());
    assert_eq!(h.get("/s/main").await.status, StatusCode::NOT_FOUND);
    assert_eq!(h.get("/s/nosuch").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_password_decides_which_site_a_login_belongs_to() {
    // t1 exists at main with pw1; the annex has its own team.
    let h = Harness::new(CONFIG);
    let main = h.sign_in("t1", "pw1").await;
    assert!(h.get_as("/print", &main).await.text().contains("Main Hall"));

    let annex = h.sign_in("a1", "pwa").await;
    assert!(h
        .get_as("/print", &annex)
        .await
        .text()
        .contains("Annex One"));

    // A site hint in the form is ignored; only the credentials count.
    let res = h
        .post_form("/login", "username=t1&password=pw1&site=annex", None)
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let session = res
        .set_cookie()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(h
        .get_as("/print", &session)
        .await
        .text()
        .contains("Main Hall"));
}

#[tokio::test]
async fn wrong_credentials_are_refused() {
    let h = Harness::new(CONFIG);
    for body in [
        "username=t1&password=wrong",
        "username=nobody&password=pw1",
        "username=&password=",
    ] {
        let res = h.post_form("/login", body, None).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(res.text().contains("Wrong login or password"));
        assert!(res.set_cookie().is_none(), "no cookie on failure");
        assert!(
            !res.text().contains("several sites"),
            "there is no ambiguous case any more"
        );
    }
}

#[tokio::test]
async fn a_valid_sign_in_issues_a_day_long_session() {
    let h = Harness::new(CONFIG);
    let res = h
        .post_form("/login", "username=t1&password=pw1", None)
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let cookie = res.set_cookie().unwrap();
    // 24 hours, as the default session length.
    assert!(cookie.contains("Max-Age=86400"), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");

    let session = cookie.split(';').next().unwrap();
    let res = h.get_as("/print", session).await;
    assert_eq!(res.status, StatusCode::OK);
    let body = res.text();
    assert!(body.contains("Team One"));
    assert!(body.contains("Main Hall"));
    assert!(body.contains("Text to print"));

    // A signed-in team is bounced away from the sign-in page.
    assert_eq!(
        h.get_as("/login", session).await.status,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn a_forged_cookie_is_rejected() {
    let h = Harness::new(CONFIG);
    let good = h.sign_in("t1", "pw1").await;
    let (name, value) = good.split_once('=').unwrap();

    // Flip a character in the signature.
    let mut tampered = value.to_string();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });

    for bad in [
        format!("{name}={tampered}"),
        format!("{name}=garbage"),
        format!("{name}="),
    ] {
        let res = h.get_as("/print", &bad).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{bad}");
        assert_eq!(res.location(), "/login");
    }

    // A cookie signed with a different secret does not transfer.
    let other = Harness::new(&CONFIG.replace("test-secret", "other-secret"));
    let foreign = other.sign_in("t1", "pw1").await;
    assert_eq!(h.get_as("/print", &foreign).await.location(), "/login");
}

#[tokio::test]
async fn signing_out_clears_the_cookie() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    let res = h.post_form("/logout", "", Some(&session)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/login");
    let cookie = res.set_cookie().unwrap();
    assert!(cookie.contains("Max-Age=0"), "{cookie}");
}

#[tokio::test]
async fn printing_queues_a_job_and_shows_it_to_the_team() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;

    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({ "content": "hello printer\nsecond line\n" }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let job = res.json();
    assert_eq!(job["id"], 1);
    assert_eq!(job["pages"], 1);
    assert_eq!(job["lines"], 2);

    let res = h.get_as("/api/my/jobs", &session).await;
    assert_eq!(res.status, StatusCode::OK);
    let body = res.json();
    assert_eq!(body["jobs"][0]["id"], 1);
    assert_eq!(body["jobs"][0]["state"], "queued");
    assert_eq!(body["jobs"][0]["team"], "Team One");
    assert!(body["html"].as_str().unwrap().contains("queued"));

    // Another team does not see it.
    let other = h.sign_in("t2", "pw2").await;
    assert_eq!(
        h.get_as("/api/my/jobs", &other).await.json()["jobs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn submissions_are_validated() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;

    for empty in ["", "   ", "\n\n"] {
        let res = h
            .post_json(
                "/api/print",
                serde_json::json!({ "content": empty }),
                Some(&session),
            )
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{empty:?}");
        assert_eq!(res.json()["error"], "Nothing to print.");
    }

    // Length is limited by pages, not by characters: a wide paste that still
    // fits on a page is accepted.
    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({ "content": "x".repeat(5_000) }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn an_oversized_job_is_refused_rather_than_cut_short() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;

    // Far more than the 20-page limit: printing the first 20 pages of it
    // would waste paper and leave the team with half a listing.
    let content: String = (0..4_000).map(|i| format!("line {i}\n")).collect();
    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({ "content": content }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", res.text());
    let error = res.json()["error"].as_str().unwrap().to_string();
    // The message names the real length, so the team can judge the cut.
    assert!(error.contains("the limit is 10"), "{error}");
    let pages: usize = error
        .split_whitespace()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no page count in {error:?}"));
    assert!(pages > 10, "{error}");

    // Nothing was queued.
    let jobs = h.get_as("/api/my/jobs", &session).await.json();
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 0);

    // A job that fits is accepted, and reports its page count.
    let content: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({ "content": content }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["pages"], 4);
}

#[tokio::test]
async fn a_team_cannot_flood_the_queue() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    let body = serde_json::json!({ "content": "print me" });

    for n in 1..=2 {
        let res = h
            .post_json("/api/print", body.clone(), Some(&session))
            .await;
        assert_eq!(res.status, StatusCode::OK, "job {n}");
    }
    let res = h
        .post_json("/api/print", body.clone(), Some(&session))
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(res.json()["error"]
        .as_str()
        .unwrap()
        .contains("waiting to print"));

    // A different team is unaffected by its neighbour's backlog.
    let other = h.sign_in("t2", "pw2").await;
    assert_eq!(
        h.post_json("/api/print", body, Some(&other)).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_form_fallback_works_without_javascript() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;

    let res = h
        .post_form("/print", "content=plain+form+post", Some(&session))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/print?ok=1&pages=1");

    // The redirect target renders the confirmation.
    let res = h.get_as("/print?ok=1&pages=1", &session).await;
    assert!(res.text().contains("Job 1 queued"));

    // And an error comes back the same way.
    let res = h.post_form("/print", "content=", Some(&session)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().starts_with("/print?error="));
    let res = h.get_as(&res.location(), &session).await;
    assert!(res.text().contains("Nothing to print."));
}

#[tokio::test]
async fn the_client_api_requires_the_site_token() {
    let h = Harness::new(CONFIG);
    let claim = serde_json::json!({ "client": "printer-1", "wait_ms": 0 });

    let res = h
        .send(
            Request::post("/api/v1/claim")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(claim.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.json()["error"]
        .as_str()
        .unwrap()
        .contains("missing site token"));

    let res = h.post_token("/api/v1/claim", "wrong", claim.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.json()["error"], "invalid site token");

    // A valid token with an empty queue parks and then reports no work.
    let res = h.post_token("/api/v1/claim", "tok-main", claim).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);

    let res = h.get_token("/api/v1/ping", "tok-main").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["site"], "main");
    assert_eq!(res.json()["name"], "Main Hall");

    // A bare ping does not invent a client of its own...
    let stats = h.get_token("/api/v1/stats", "tok-main").await.json();
    let before = stats["clients"].as_array().unwrap().len();
    let names: Vec<&str> = stats["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"ping"), "{names:?}");

    // ...but a ping that carries a name registers it straight away, so the
    // client shows up before it has taken any work.
    h.get_token("/api/v1/ping?client=printer-9", "tok-main")
        .await;
    let stats = h.get_token("/api/v1/stats", "tok-main").await.json();
    let clients = stats["clients"].as_array().unwrap();
    assert_eq!(clients.len(), before + 1);
    let registered = clients
        .iter()
        .find(|c| c["name"] == "printer-9")
        .expect("the named client is listed");
    assert_eq!(registered["claimed"], 0);
}

#[tokio::test]
async fn a_client_claims_prints_and_confirms() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    h.post_json(
        "/api/print",
        serde_json::json!({ "content": "job content here" }),
        Some(&session),
    )
    .await;

    let res = h
        .post_token(
            "/api/v1/claim",
            "tok-main",
            serde_json::json!({ "client": "printer-1" }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let job = &res.json()["job"];
    assert_eq!(job["id"], 1);
    assert_eq!(job["team"], "Team One");
    assert_eq!(job["username"], "t1");
    assert_eq!(job["site"], "main");
    assert_eq!(job["attempt"], 1);
    assert_eq!(job["pdf_url"], "/api/v1/jobs/1/pdf");

    // The document itself, built on request from the submitted text.
    let res = h.get_token("/api/v1/jobs/1/pdf", "tok-main").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.headers.get(header::CONTENT_TYPE).unwrap(),
        "application/pdf"
    );
    assert!(res.body.starts_with(b"%PDF-1.7"), "not a PDF");
    assert!(res.body.ends_with(b"%%EOF\n"));
    // Rendering is deterministic, so a client that retries gets the same
    // document rather than a different one.
    let again = h.get_token("/api/v1/jobs/1/pdf", "tok-main").await;
    assert_eq!(again.body, res.body, "a repeated fetch must be identical");

    // While printing, the team sees the state change.
    let jobs = h.get_as("/api/my/jobs", &session).await.json();
    assert_eq!(jobs["jobs"][0]["state"], "printing");

    let res = h
        .post_token(
            "/api/v1/jobs/1/complete",
            "tok-main",
            serde_json::json!({ "ok": true }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["job"]["state"], "printed");

    // Confirming twice is refused rather than double counted.
    let res = h
        .post_token(
            "/api/v1/jobs/1/complete",
            "tok-main",
            serde_json::json!({ "ok": true }),
        )
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);

    let stats = h.get_token("/api/v1/stats", "tok-main").await.json();
    assert_eq!(stats["processed"], 1);
    // The zone travels with the stats, so the client needs no flag for it.
    assert_eq!(stats["timezone"], "UTC");
    assert_eq!(stats["queued"], 0);
    assert_eq!(stats["failed"], 0);
    assert_eq!(stats["recent"][0]["state"], "printed");
    assert_eq!(stats["clients"][0]["name"], "printer-1");
    assert_eq!(stats["clients"][0]["claimed"], 1);

    // The text is released once the job is done, so no document can be built.
    assert_eq!(
        h.get_token("/api/v1/jobs/1/pdf", "tok-main").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_failed_print_is_reported_to_the_team() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    h.post_json(
        "/api/print",
        serde_json::json!({ "content": "x" }),
        Some(&session),
    )
    .await;
    h.post_token(
        "/api/v1/claim",
        "tok-main",
        serde_json::json!({ "client": "printer-1" }),
    )
    .await;

    let res = h
        .post_token(
            "/api/v1/jobs/1/complete",
            "tok-main",
            serde_json::json!({ "ok": false, "error": "lp: The printer is not responding." }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);

    let jobs = h.get_as("/api/my/jobs", &session).await.json();
    assert_eq!(jobs["jobs"][0]["state"], "failed");
    assert_eq!(
        jobs["jobs"][0]["error"],
        "lp: The printer is not responding."
    );
    assert!(jobs["html"].as_str().unwrap().contains("not responding"));

    let stats = h.get_token("/api/v1/stats", "tok-main").await.json();
    assert_eq!(stats["failed"], 1);
    assert_eq!(stats["processed"], 0);
}

#[tokio::test]
async fn two_clients_sharing_a_token_split_the_queue() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    for _ in 0..2 {
        assert_eq!(
            h.post_json(
                "/api/print",
                serde_json::json!({ "content": "shared queue" }),
                Some(&session)
            )
            .await
            .status,
            StatusCode::OK
        );
    }

    // Two clients, one token: the jobs are split, not duplicated.
    let first = h
        .post_token(
            "/api/v1/claim",
            "tok-main",
            serde_json::json!({ "client": "printer-a" }),
        )
        .await;
    let second = h
        .post_token(
            "/api/v1/claim",
            "tok-main",
            serde_json::json!({ "client": "printer-b" }),
        )
        .await;
    assert_eq!(first.json()["job"]["id"], 1);
    assert_eq!(second.json()["job"]["id"], 2);

    // Nothing is left for a third poll.
    let third = h
        .post_token(
            "/api/v1/claim",
            "tok-main",
            serde_json::json!({ "client": "printer-c", "wait_ms": 0 }),
        )
        .await;
    assert_eq!(third.status, StatusCode::NO_CONTENT);

    // All three are visible to the operator, with what each has taken.
    let stats = h.get_token("/api/v1/stats", "tok-main").await.json();
    let clients = stats["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 3);
    let taken: std::collections::BTreeMap<&str, u64> = clients
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c["claimed"].as_u64().unwrap()))
        .collect();
    assert_eq!(taken["printer-a"], 1);
    assert_eq!(taken["printer-b"], 1);
    assert_eq!(taken["printer-c"], 0);

    // And the operator page explains what sharing a token means.
    let page = h.get("/stats?token=adm-token").await.text();
    assert!(page.contains("printer-a"), "{page}");
    assert!(page.contains("share the queue"), "{page}");
}

#[tokio::test]
async fn sites_cannot_reach_each_others_jobs() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    h.post_json(
        "/api/print",
        serde_json::json!({ "content": "main hall only" }),
        Some(&session),
    )
    .await;

    // The annex client must not receive the main hall's job.
    let res = h
        .post_token(
            "/api/v1/claim",
            "tok-annex",
            serde_json::json!({ "client": "annex-printer", "wait_ms": 0 }),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);

    // Nor fetch its document, even knowing the id.
    assert_eq!(
        h.get_token("/api/v1/jobs/1/pdf", "tok-annex").await.status,
        StatusCode::NOT_FOUND
    );
    // Nor confirm it.
    let res = h
        .post_token(
            "/api/v1/jobs/1/complete",
            "tok-annex",
            serde_json::json!({ "ok": true }),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // Stats stay separate.
    let annex = h.get_token("/api/v1/stats", "tok-annex").await.json();
    assert_eq!(annex["site"], "annex");
    assert_eq!(annex["timezone"], "UTC+05:00");
    assert_eq!(annex["queued"], 0);
    let main = h.get_token("/api/v1/stats", "tok-main").await.json();
    assert_eq!(main["queued"], 1);
}

#[tokio::test]
async fn stats_are_exposed_per_site_and_across_sites() {
    let h = Harness::new(CONFIG);

    // Per-site: the site token is enough.
    let res = h.get_token("/api/v1/stats", "tok-main").await;
    assert_eq!(res.status, StatusCode::OK);
    let stats = res.json();
    assert_eq!(stats["site"], "main");
    assert_eq!(stats["name"], "Main Hall");
    // t1, t2 and t3 - t3 is listed twice, for its two passwords.
    assert_eq!(stats["teams"], 3);
    assert_eq!(stats["queued"], 0);
    assert_eq!(stats["processed"], 0);

    // Cross-site: admin token only.
    assert_eq!(
        h.get("/api/v1/admin/stats").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.get_token("/api/v1/admin/stats", "tok-main").await.status,
        StatusCode::UNAUTHORIZED
    );
    let res = h.get_token("/api/v1/admin/stats", "adm-token").await;
    assert_eq!(res.status, StatusCode::OK);
    let sites = res.json();
    assert_eq!(sites["sites"].as_array().unwrap().len(), 2);
    assert_eq!(sites["sites"][0]["site"], "annex");
    assert_eq!(sites["sites"][0]["timezone"], "UTC+05:00");
    assert_eq!(sites["sites"][1]["site"], "main");
    assert_eq!(sites["sites"][1]["timezone"], "UTC");

    // The HTML page is behind the same token, also accepted as a query param.
    assert_eq!(h.get("/stats").await.status, StatusCode::UNAUTHORIZED);
    let res = h.get("/stats?token=adm-token").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.text().contains("Main Hall"));
    assert!(res.text().contains("in queue"));
}

#[tokio::test]
async fn stats_are_disabled_when_no_admin_token_is_configured() {
    let h = Harness::new(&CONFIG.replace("admin_token: \"adm-token\"", ""));
    let res = h.get("/stats?token=anything").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.json()["error"]
        .as_str()
        .unwrap()
        .contains("admin_token"));
}

#[tokio::test]
async fn a_long_poll_delivers_a_job_submitted_while_waiting() {
    let h = std::sync::Arc::new(Harness::new(CONFIG));
    let session = h.sign_in("t1", "pw1").await;

    let submitter = {
        let h = h.clone();
        let session = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            h.post_json(
                "/api/print",
                serde_json::json!({ "content": "late arrival" }),
                Some(&session),
            )
            .await
            .status
        })
    };

    let started = std::time::Instant::now();
    let res = h
        .post_token(
            "/api/v1/claim",
            "tok-main",
            serde_json::json!({ "client": "printer-1", "wait_ms": 5000 }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["job"]["id"], 1);
    // It returned as soon as the job arrived, not after the full wait.
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(submitter.await.unwrap(), StatusCode::OK);
}

#[tokio::test]
async fn highlighting_is_offered_and_applied() {
    let h = Harness::with_highlighting(CONFIG);
    let session = h.sign_in("t1", "pw1").await;

    // Nothing to choose: the page just takes the text.
    let page = h.get_as("/print", &session).await.text();
    assert!(
        !page.contains("<select"),
        "the language picker should be gone"
    );
    assert!(page.contains("pages per job"), "{page}");

    // The language is worked out from the content itself.
    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({ "content": "#include <bits/stdc++.h>\nint main() { return 0; }" }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Fetch the rendered document and confirm the bold face got embedded.
    h.post_token(
        "/api/v1/claim",
        "tok-main",
        serde_json::json!({ "client": "c" }),
    )
    .await;
    let pdf = h.get_token("/api/v1/jobs/1/pdf", "tok-main").await;
    let raw = String::from_utf8_lossy(&pdf.body);
    assert!(raw.contains("/JetBrainsMono"), "font not embedded");
    assert!(raw.contains("/F2 "), "bold face missing for keywords");
}

/// The same configuration, proxied under `/print`.
fn sub_path_config() -> String {
    format!("base_path: /print\n{CONFIG}")
}

#[tokio::test]
async fn under_a_prefix_pages_link_to_the_prefix() {
    let h = Harness::new(&sub_path_config());

    // nginx `proxy_pass http://127.0.0.1:12345/;` strips the prefix, so the
    // server sees the bare path but must still emit public URLs.
    let res = h.get("/login").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.text().contains("action=\"/print/login\""),
        "{}",
        res.text()
    );

    // A proxy that passes the prefix through reaches the same page.
    let res = h.get("/print/login").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.text().contains("action=\"/print/login\""));

    // Unauthenticated redirects point at the public path.
    assert_eq!(h.get("/").await.location(), "/print/login");
    assert_eq!(h.get("/print/").await.location(), "/print/login");
    assert_eq!(h.get("/print").await.location(), "/print/login");
}

#[tokio::test]
async fn a_whole_session_works_under_a_prefix() {
    let h = Harness::new(&sub_path_config());

    // Sign in through the prefixed path, as the browser would.
    let res = h
        .post_form("/print/login", "username=t1&password=pw1", None)
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.text());
    assert_eq!(res.location(), "/print/print");

    let cookie = res.set_cookie().expect("a session cookie");
    // Scoped to the prefix, so it is not sent to the rest of the host.
    assert!(cookie.contains("Path=/print;"), "{cookie}");
    let session = cookie.split(';').next().unwrap().to_string();

    // The print page and its JSON endpoints answer under the prefix.
    let page = h.get_as("/print/print", &session).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("'/print/api/print'"));

    let res = h
        .post_json(
            "/print/api/print",
            serde_json::json!({ "content": "proxied job" }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], 1);

    let jobs = h.get_as("/print/api/my/jobs", &session).await;
    assert_eq!(jobs.status, StatusCode::OK);
    assert_eq!(jobs.json()["jobs"][0]["id"], 1);

    // The no-JavaScript form round trip keeps the prefix too.
    let res = h
        .post_form("/print/print", "content=second+job", Some(&session))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/print/print?ok=2&pages=1");

    // And signing out clears the cookie on the same path.
    let res = h.post_form("/print/logout", "", Some(&session)).await;
    assert!(res.set_cookie().unwrap().contains("Path=/print;"));
    assert_eq!(res.location(), "/print/login");
}

#[tokio::test]
async fn the_client_api_works_with_or_without_the_prefix() {
    let h = Harness::new(&sub_path_config());
    let session = h.sign_in("t1", "pw1").await;
    h.post_json(
        "/api/print",
        serde_json::json!({ "content": "for the printer" }),
        Some(&session),
    )
    .await;

    // A client pointed at https://example.org/print reaches the nested routes.
    let res = h
        .post_token(
            "/print/api/v1/claim",
            "tok-main",
            serde_json::json!({ "client": "printer-1" }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let job = &res.json()["job"];
    assert_eq!(job["id"], 1);

    // The document URL stays relative to the client's own base, so it must
    // not carry the prefix itself.
    assert_eq!(job["pdf_url"], "/api/v1/jobs/1/pdf");

    // Both spellings fetch it, because either proxy style is possible.
    for path in ["/print/api/v1/jobs/1/pdf", "/api/v1/jobs/1/pdf"] {
        let res = h.get_token(path, "tok-main").await;
        assert_eq!(res.status, StatusCode::OK, "{path}");
        assert!(res.body.starts_with(b"%PDF"), "{path}");
    }

    let res = h
        .post_token(
            "/print/api/v1/jobs/1/complete",
            "tok-main",
            serde_json::json!({ "ok": true }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        h.get_token("/print/api/v1/stats", "tok-main").await.json()["processed"],
        1
    );
    assert_eq!(h.get("/print/healthz").await.status, StatusCode::OK);
}

#[tokio::test]
async fn without_a_prefix_nothing_changes() {
    let h = Harness::new(CONFIG);
    let res = h.get("/login").await;
    assert!(res.text().contains("action=\"/login\""), "{}", res.text());
    assert_eq!(h.get("/").await.location(), "/login");
    // The prefixed paths simply do not exist.
    assert_eq!(h.get("/print/login").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn each_site_shows_times_in_its_own_zone() {
    // The server-wide offset is 0; the annex sets +5.
    let h = Harness::new(CONFIG);

    let main = h.sign_in("t1", "pw1").await;
    let annex = h.sign_in("a1", "pwa").await;
    let body = serde_json::json!({ "content": "same instant, two venues" });
    assert_eq!(
        h.post_json("/api/print", body.clone(), Some(&main))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        h.post_json("/api/print", body, Some(&annex)).await.status,
        StatusCode::OK
    );

    let hour_of = |value: &serde_json::Value| -> i64 {
        let html = value["html"].as_str().unwrap();
        // The table renders HH:MM:SS in the site's own offset.
        let cell = html
            .split("<td>")
            .nth(1)
            .and_then(|c| c.split("</td>").next())
            .expect("a time cell");
        cell.split(':').next().unwrap().parse().expect("an hour")
    };

    let main_jobs = h.get_as("/api/my/jobs", &main).await.json();
    let annex_jobs = h.get_as("/api/my/jobs", &annex).await.json();
    // Both jobs were submitted at the same instant.
    let created: i64 = main_jobs["jobs"][0]["created_at"].as_i64().unwrap();
    assert!((created - annex_jobs["jobs"][0]["created_at"].as_i64().unwrap()).abs() <= 1);

    // The displayed hours differ by the configured five.
    let diff = (hour_of(&annex_jobs) - hour_of(&main_jobs)).rem_euclid(24);
    assert_eq!(diff, 5, "annex should run five hours ahead of the server");
}

#[tokio::test]
async fn a_submitted_language_is_ignored_rather_than_obeyed() {
    // Older clients, or a crafted request, may still send one. Detection is
    // the only thing that decides, so the field has no effect.
    let h = Harness::with_highlighting(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({
                "content": "import sys\nprint(sys.stdin.read())",
                "language": "plain",
            }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    h.post_token(
        "/api/v1/claim",
        "tok-main",
        serde_json::json!({ "client": "c" }),
    )
    .await;
    let pdf = h.get_token("/api/v1/jobs/1/pdf", "tok-main").await;
    let raw = String::from_utf8_lossy(&pdf.body);
    // Python was detected and highlighted: keywords pulled in the bold face.
    assert!(
        raw.contains("/F2 "),
        "the content should still be highlighted"
    );
}

#[tokio::test]
async fn unicode_content_round_trips_through_the_form_encoding() {
    let h = Harness::new(CONFIG);
    let session = h.sign_in("t1", "pw1").await;
    let res = h
        .post_json(
            "/api/print",
            serde_json::json!({ "content": "Привет, мир!\nελληνικά\n" }),
            Some(&session),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["lines"], 2);
}
