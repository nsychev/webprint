//! webprint server.

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use webprint_pdf::{PageGeometry, Renderer, Style};

mod auth;
mod config;
mod journal;
mod queue;
mod routes;
mod state;
mod timezone;
mod util;
mod views;

use config::Config;

/// How long connections are given to finish once shutdown begins.
const GRACE_PERIOD: Duration = Duration::from_secs(5);

use state::AppState;

#[derive(Parser, Debug)]
#[command(
    name = "webprint-server",
    version,
    about = "Print server for distributed ICPC-style contests"
)]
struct Args {
    /// Path to the YAML configuration file.
    #[arg(short, long, default_value = "config.yaml", env = "WEBPRINT_CONFIG")]
    config: PathBuf,

    /// Override the listen address from the configuration.
    #[arg(short, long, env = "WEBPRINT_LISTEN")]
    listen: Option<String>,

    /// Public path prefix when proxied below the root, e.g. /print.
    #[arg(long, env = "WEBPRINT_BASE_PATH")]
    base_path: Option<String>,

    /// Validate the configuration and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WEBPRINT_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = Config::load(&args.config)?;
    if let Some(base_path) = args.base_path.clone() {
        config.base_path = Some(base_path);
        config.validate()?;
    }

    if config.admin_token.is_none() {
        tracing::warn!("admin_token is not set: the /stats page is disabled");
    }
    if config.secret.is_none() {
        tracing::warn!("secret is not set: a random key is used, so sessions end on restart");
    }

    if args.check {
        // A login listed twice is one team with two passwords, so count
        // logins rather than configuration entries.
        let teams: usize = config
            .sites
            .values()
            .map(|site| {
                site.teams
                    .iter()
                    .map(|t| t.username.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            })
            .sum();
        let entries: usize = config.sites.values().map(|s| s.teams.len()).sum();
        let extra = if entries > teams {
            format!(", {entries} credentials")
        } else {
            String::new()
        };
        println!(
            "configuration is valid: {} site(s), {teams} team(s){extra}",
            config.sites.len(),
        );
        return Ok(());
    }

    let renderer = build_renderer(&config)?;
    tracing::info!(
        columns = renderer.text_columns(100),
        max_lines = config.limits.max_lines,
        "pdf renderer ready"
    );

    let listen = args.listen.unwrap_or_else(|| config.listen.clone());
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid listen address {listen:?}"))?;

    if !config.base_path().is_empty() {
        tracing::info!(
            base_path = %config.base_path(),
            "serving under a path prefix (and at the root, for proxies that strip it)"
        );
    }

    let state_path = config.state_path(&args.config);
    let state = AppState::with_store(config, renderer, |config| {
        let store = queue::Store::new(config);
        // The queue is kept on disk so a restart does not lose it. Failing to
        // open the file is worth shouting about, but not worth refusing to
        // print over.
        match journal::Journal::open(&state_path) {
            Ok((journal, events)) => {
                tracing::info!(
                    path = %state_path.display(),
                    events = events.len(),
                    "queue state file"
                );
                store.with_journal(journal, events)
            }
            Err(e) => {
                tracing::error!(
                    path = %state_path.display(),
                    error = %e,
                    "could not open the state file; the queue will not survive a restart"
                );
                store
            }
        }
    });

    let sweeper = spawn_lease_sweeper(state.clone());
    let stopping = state.clone();
    let mut grace = state.shutdown.clone();
    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "webprint server listening");

    let served = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown_signal().await;
        // Tell requests that are deliberately held open - a client's long
        // poll - to finish now, instead of waiting out their timeout.
        stopping.begin_shutdown();
    });
    // `WithGracefulShutdown` is only `IntoFuture`, and select! needs to poll
    // it more than once.
    let served = served.into_future();
    tokio::pin!(served);

    // A connection that still will not finish must not hold the process. The
    // grace period starts when shutdown does, not when the server starts.
    tokio::select! {
        result = &mut served => result.context("server error")?,
        _ = async {
            let _ = grace.wait_for(|stopping| *stopping).await;
            tokio::time::sleep(GRACE_PERIOD).await;
        } => tracing::warn!("a connection did not close in time; stopping anyway"),
    }

    sweeper.abort();
    tracing::info!("shutdown complete");
    Ok(())
}

/// Builds the PDF renderer from the configuration.
fn build_renderer(config: &Config) -> Result<Renderer> {
    let pdf = &config.pdf;

    let mut renderer = match &pdf.syntax_dir {
        Some(dir) => {
            let highlighter = webprint_pdf::highlight::Highlighter::with_extra_syntaxes(dir)
                .with_context(|| format!("loading extra syntaxes from {}", dir.display()))?;
            tracing::info!(
                dir = %dir.display(),
                grammars = highlighter.syntax_names().len(),
                "loaded extra syntax definitions"
            );
            Renderer::new().with_highlighter(highlighter)
        }
        None => Renderer::with_highlighting(),
    };

    if let Some(path) = &pdf.font_path {
        let fonts = load_fonts(path, pdf.font_bold_path.as_deref())?;
        renderer = renderer.with_fonts(fonts);
    }

    Ok(renderer
        .with_geometry(PageGeometry::a4())
        .with_style(Style {
            font_size: pdf.font_size,
            line_spacing: pdf.line_spacing,
            tab_width: pdf.tab_width,
            line_numbers: pdf.line_numbers,
            highlight: pdf.highlight,
            ..Style::default()
        }))
}

/// Loads replacement fonts; the bold face falls back to the regular one.
fn load_fonts(
    regular: &std::path::Path,
    bold: Option<&std::path::Path>,
) -> Result<webprint_pdf::FontSet> {
    let read = |p: &std::path::Path| -> Result<webprint_pdf::Font> {
        let bytes = std::fs::read(p).with_context(|| format!("reading font {}", p.display()))?;
        webprint_pdf::Font::from_bytes(bytes).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))
    };
    let regular = read(regular)?;
    let bold = match bold {
        Some(p) => read(p)?,
        None => regular.clone(),
    };
    Ok(webprint_pdf::FontSet {
        italic: regular.clone(),
        bold_italic: bold.clone(),
        regular,
        bold,
    })
}

/// Requeues jobs whose print client went away mid-job.
fn spawn_lease_sweeper(state: AppState) -> tokio::task::JoinHandle<()> {
    let store = Arc::clone(&state.store);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let touched = store.sweep(util::now_unix());
            if touched > 0 {
                tracing::info!(jobs = touched, "requeued or failed stale jobs");
            }
        }
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("installing the Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing the SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl-C, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
