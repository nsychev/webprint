//! webprint onsite print client.
//!
//! Polls the server for print jobs and sends them to the printer. Runs on
//! Windows and on Unix; see [`printer`] for how each platform prints.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

mod api;
mod printer;
mod ui;

use api::{Api, ClaimedJob};
use ui::{now_unix, Emphasis, Status};
use universal_print::Sink;

#[derive(Parser, Debug)]
#[command(
    name = "webprint-client",
    version,
    about = "Print client for webprint: fetches print jobs and prints them",
    // `printers` needs no required arguments
    subcommand_negates_reqs = true
)]
struct Args {
    /// Server base URL
    #[arg(
        short,
        long,
        env = "WEBPRINT_SERVER",
        default_value = DEFAULT_SERVER,
        required = DEFAULT_SERVER.is_none()
    )]
    server: Option<String>,

    /// Site token
    #[arg(short, long, env = "WEBPRINT_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Printer name; the system default is used when omitted.
    /// Incompatible with IPP backend.
    #[arg(short, long, env = "WEBPRINT_PRINTER")]
    printer: Option<String>,

    /// Printer backend: auto, lp, lpr, sumatra[:path], winspooler, ipp:// /
    /// ipps:// / http:// / https:// URI for a network printer or
    /// command:<template> for a custom command.
    #[arg(long, env = "WEBPRINT_BACKEND", default_value = "auto")]
    backend: String,

    /// Name reported to the server, shown in its stats, hostname by default.
    #[arg(long, env = "WEBPRINT_CLIENT_NAME")]
    name: Option<String>,

    /// Seconds the server may hold a poll open while waiting for work.
    #[arg(long, default_value_t = 15)]
    poll_seconds: u64,

    /// Seconds to allow the print command before giving up on a job.
    #[arg(long, default_value_t = 120)]
    print_timeout: u64,

    /// Also keep a copy of every printed PDF in this directory.
    #[arg(long)]
    keep: Option<PathBuf>,

    /// Print plain log lines instead of the live status panel.
    #[arg(long)]
    no_ui: bool,

    /// Handle at most one poll cycle, then exit.
    #[arg(long)]
    once: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Poll for jobs and print them (the default).
    Run,
    /// List the printers this machine can see.
    Printers,
}

/// If `WEBPRINT_DEFAULT_SERVER` is set at build time, the client will use that server
/// when no `--server` is given.
const DEFAULT_SERVER: Option<&str> = option_env!("WEBPRINT_DEFAULT_SERVER");

static STOPPING: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
/// True while the status panel owns the terminal. Terminal control sequences
/// are only emitted then, so log mode stays clean for a log file.
static LIVE_UI: AtomicBool = AtomicBool::new(false);

fn main() {
    let args = Args::parse();
    let interactive = std::io::stdin().is_terminal();

    let result = match args.command {
        Some(Command::Printers) => list_printers(&args),
        Some(Command::Run) | None => run(args),
    };

    if let Err(error) = result {
        eprintln!("Error: {error:#}");

        if interactive {
            eprint!("\nPress Enter to close. ");
            let _ = std::io::stderr().flush();
            let mut discard = String::new();
            let _ = std::io::stdin().read_line(&mut discard);
        }
        std::process::exit(1);
    }
}

/// Resolves the printer backend.
fn open_sink(args: &Args) -> Result<(Sink, Option<String>)> {
    api::install_tls_provider(); // For IPPS

    let named = args.printer.clone().filter(|p| !p.is_empty());
    let sink = printer::open(
        &args.backend,
        named.as_deref(),
        Duration::from_secs(args.print_timeout),
    )?;
    let printer = named.or_else(|| printer::default_printer(&sink));
    Ok((sink, printer))
}

/// Asks for anything that was not passed on the command line.
fn resolve_credentials(args: &Args) -> Result<(String, String)> {
    let interactive = std::io::stdin().is_terminal();

    let server = match args.server.clone().filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => bail!("no server given: pass --server or set WEBPRINT_SERVER"),
    };

    let token = match args.token.clone() {
        Some(t) if !t.is_empty() => t,
        _ if interactive => {
            // Read without echo: the token is a shared secret for the site.
            let token =
                rpassword::prompt_password("Site token: ").context("reading the site token")?;
            if token.trim().is_empty() {
                bail!("no site token given");
            }
            token.trim().to_string()
        }
        _ => bail!("no site token given: pass --token or set WEBPRINT_TOKEN"),
    };

    Ok((server, token))
}

fn client_name(args: &Args) -> String {
    args.name.clone().unwrap_or_else(|| {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty())
            })
            .unwrap_or_else(|| "webprint-client".to_string())
    })
}

fn connect(args: &Args) -> Result<(Api, String)> {
    let (server, token) = resolve_credentials(args)?;
    let name = client_name(args);
    let api = Api::new(
        &server,
        &token,
        &name,
        Duration::from_secs(args.poll_seconds),
    )?;
    Ok((api, name))
}

fn list_printers(args: &Args) -> Result<()> {
    let sink = printer::open(&args.backend, None, printer::DEFAULT_TIMEOUT)?;
    println!("Printing backend: {}", printer::label(&sink));
    let printers = printer::printers(&printer::runtime()?, &sink)?;
    if printers.is_empty() {
        println!("No printers found.");
    }
    for p in &printers {
        let default = if p.is_default { "  (default)" } else { "" };
        let driver = p.driver.as_deref().map(|d| format!("  [{d}]"));
        println!("  {}{default}{}", p.name, driver.unwrap_or_default());
    }
    Ok(())
}

fn run(args: Args) -> Result<()> {
    let (sink, printer_name) = open_sink(&args)?;
    let rt = printer::runtime()?;

    if let Err(e) = printer::check(&rt, &sink) {
        eprintln!("warning: the printer did not confirm that it takes PDFs: {e:#}");
    }
    let (api, name) = connect(&args)?;

    // Confirm the token before settling into the poll loop, so a typo is
    // reported immediately instead of looking like a network problem.
    let ping = match api.ping() {
        Ok(ping) => ping,
        Err(e) if e.is_auth() => bail!("the server rejected the site token: {e}"),
        Err(e) => {
            eprintln!(
                "warning: cannot reach {} yet ({e}); retrying in the background",
                api.base()
            );
            api::Ping {
                site: String::new(),
                name: String::new(),
            }
        }
    };

    let status = Arc::new(Mutex::new({
        let mut s = Status::new(
            api.base().to_string(),
            name.clone(),
            printer::label(&sink),
            Some(
                printer_name
                    .clone()
                    .unwrap_or_else(|| printer::unnamed_printer(&sink).into()),
            ),
        );
        if !ping.site.is_empty() {
            s.site = Some((ping.name.clone(), ping.site.clone()));
            s.mark_online();
        }
        s
    }));

    ctrlc::set_handler(|| {
        // A second Ctrl-C, or one while idle, exits at once; otherwise let the
        // in-flight job finish so the server hears about it.
        if STOPPING.swap(true, Ordering::SeqCst) || !BUSY.load(Ordering::SeqCst) {
            show_cursor();
            println!();
            std::process::exit(0);
        }
        eprintln!("\nfinishing the current job, Ctrl-C again to stop now");
    })
    .context("installing the Ctrl-C handler")?;

    let live_ui = !args.no_ui && std::io::stdout().is_terminal();
    LIVE_UI.store(live_ui, Ordering::SeqCst);
    let painter = live_ui.then(|| spawn_painter(status.clone()));
    if !live_ui {
        println!(
            "webprint-client {} - server {}, printer {}, backend {}",
            env!("CARGO_PKG_VERSION"),
            api.base(),
            printer_name
                .clone()
                .unwrap_or_else(|| printer::unnamed_printer(&sink).into()),
            printer::label(&sink)
        );
    }

    let poll_wait = Duration::from_secs(args.poll_seconds);

    while !STOPPING.load(Ordering::SeqCst) {
        // Refresh the counters and recent jobs shown in the panel.
        match api.stats() {
            Ok(stats) => {
                let mut s = status.lock().unwrap();
                s.site = Some((stats.name.clone(), stats.site.clone()));
                s.recent = stats.recent.iter().take(8).cloned().collect();
                // Times are shown in the site's own zone, which the server
                // knows, so the client needs no setting of its own.
                s.tz_minutes = ui::parse_offset_minutes(stats.timezone.as_deref().unwrap_or("UTC"));
                s.stats = Some(stats);
                s.mark_online();
            }
            Err(e) if e.is_auth() => {
                stop_painter(painter);
                bail!("the server rejected the site token: {e}");
            }
            Err(e) => {
                status
                    .lock()
                    .unwrap()
                    .mark_offline(e.to_string(), now_unix());
            }
        }

        if STOPPING.load(Ordering::SeqCst) {
            break;
        }

        if args.once && STOPPING.load(Ordering::SeqCst) {
            break;
        }

        match api.claim(poll_wait) {
            Ok(Some(job)) => {
                BUSY.store(true, Ordering::SeqCst);
                let outcome = handle_job(
                    &api,
                    &rt,
                    &sink,
                    &job,
                    args.keep.as_deref(),
                    live_ui,
                    &status,
                );
                BUSY.store(false, Ordering::SeqCst);
                let mut s = status.lock().unwrap();
                s.current = None;
                match outcome {
                    Ok(()) => s.printed += 1,
                    Err(err) => {
                        s.failed += 1;
                        s.last_error = Some(err);
                    }
                }
            }
            Ok(None) => {
                status.lock().unwrap().mark_online();
            }
            Err(e) if e.is_auth() => {
                stop_painter(painter);
                bail!("the server rejected the site token: {e}");
            }
            Err(e) => {
                status
                    .lock()
                    .unwrap()
                    .mark_offline(e.to_string(), now_unix());
                // Back off so a down server is not hammered.
                if !args.once {
                    sleep_interruptibly(Duration::from_secs(3));
                }
            }
        }

        if args.once {
            break;
        }
    }

    stop_painter(painter);
    let s = status.lock().unwrap();
    println!(
        "\nStopped. {} job(s) printed, {} failed.",
        s.printed, s.failed
    );
    Ok(())
}

/// Fetches, prints and reports one job. The returned error is what the server
/// is told, so it should read well to a team.
fn handle_job(
    api: &Api,
    rt: &tokio::runtime::Runtime,
    sink: &Sink,
    job: &ClaimedJob,
    keep: Option<&std::path::Path>,
    live_ui: bool,
    status: &Arc<Mutex<Status>>,
) -> std::result::Result<(), String> {
    {
        let mut s = status.lock().unwrap();
        s.current = Some(format!(
            "job {} for {} ({}) - {} page{}, {} lines",
            job.id,
            job.username,
            job.team,
            job.pages,
            if job.pages == 1 { "" } else { "s" },
            job.lines
        ));
        if s.site.is_none() {
            s.site = Some((job.site.clone(), job.site.clone()));
        }
    }
    if !live_ui {
        println!(
            "job {} from {} ({}): {} page(s), {} lines, waited {}, attempt {}",
            job.id,
            job.username,
            job.team,
            job.pages,
            job.lines,
            ui::format_age(now_unix() - job.created_at),
            job.attempt
        );
    }

    let result = (|| -> std::result::Result<(), String> {
        let pdf = api
            .fetch_pdf(job)
            .map_err(|e| format!("could not download the document: {e}"))?;
        if !live_ui {
            println!("job {} document is {} KiB", job.id, pdf.len() / 1024);
        }

        if let Some(dir) = keep {
            if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| {
                std::fs::write(dir.join(format!("webprint-job-{}.pdf", job.id)), &pdf)
            }) {
                eprintln!("warning: could not keep a copy in {}: {e}", dir.display());
            }
        }

        // The sink spools the document itself where a backend needs a path,
        // and removes that file afterwards.
        printer::print(
            rt,
            sink,
            &pdf,
            &format!("webprint job {} ({})", job.id, job.team),
            Some(&job.username),
        )
        .map_err(|e| format!("{e:#}"))
    })();

    // Always report, so the job does not sit claimed until its lease expires.
    match &result {
        Ok(()) => {
            if let Err(e) = api.complete(job.id, true, None) {
                eprintln!(
                    "warning: printed job {} but could not report it: {e}",
                    job.id
                );
            } else if !live_ui {
                println!("job {} printed", job.id);
            }
        }
        Err(err) => {
            if !live_ui {
                eprintln!("job {} failed: {err}", job.id);
            }
            if let Err(e) = api.complete(job.id, false, Some(err)) {
                eprintln!(
                    "warning: could not report the failure of job {}: {e}",
                    job.id
                );
            }
        }
    }
    result
}

/// Repaints the status panel once a second.
fn spawn_painter(status: Arc<Mutex<Status>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !STOPPING.load(Ordering::SeqCst) {
            {
                let snapshot = status.lock().unwrap().clone();
                if let Err(e) = paint(&snapshot) {
                    eprintln!("status display stopped: {e}");
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    })
}

fn stop_painter(painter: Option<std::thread::JoinHandle<()>>) {
    STOPPING.store(true, Ordering::SeqCst);
    if let Some(handle) = painter {
        let _ = handle.join();
    }
    show_cursor();
}

/// Undoes the panel's `cursor::Hide`, but only if the panel ever ran.
fn show_cursor() {
    if LIVE_UI.load(Ordering::SeqCst) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
    }
}

/// Draws the panel, clearing the screen first.
fn paint(status: &Status) -> Result<()> {
    use crossterm::style::{Color, Print, ResetColor, SetAttribute, SetForegroundColor};
    use crossterm::{cursor, queue, terminal};

    let mut out = std::io::stdout();
    queue!(
        out,
        cursor::Hide,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    queue!(out, Print("\n"))?;

    for line in ui::render(status, now_unix()) {
        for (text, emphasis) in line {
            match emphasis {
                Emphasis::Plain => {
                    queue!(out, ResetColor, Print(text))?;
                }
                Emphasis::Dim => {
                    queue!(
                        out,
                        SetForegroundColor(Color::DarkGrey),
                        Print(text),
                        ResetColor
                    )?;
                }
                Emphasis::Head => queue!(
                    out,
                    SetAttribute(crossterm::style::Attribute::Bold),
                    Print(text),
                    SetAttribute(crossterm::style::Attribute::Reset)
                )?,
                Emphasis::Ok => queue!(
                    out,
                    SetForegroundColor(Color::Green),
                    Print(text),
                    ResetColor
                )?,
                Emphasis::Warn => queue!(
                    out,
                    SetForegroundColor(Color::Yellow),
                    Print(text),
                    ResetColor
                )?,
                Emphasis::Err => {
                    queue!(out, SetForegroundColor(Color::Red), Print(text), ResetColor)?
                }
            }
        }
        queue!(out, Print("\n"))?;
    }
    out.flush()?;
    Ok(())
}

/// Sleeps, but wakes up early when the client is stopping.
fn sleep_interruptibly(total: Duration) {
    let step = Duration::from_millis(200);
    let mut slept = Duration::ZERO;
    while slept < total && !STOPPING.load(Ordering::SeqCst) {
        std::thread::sleep(step);
        slept += step;
    }
}
