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
use printer::Backend;
use ui::{now_unix, Emphasis, Status};

#[derive(Parser, Debug)]
#[command(
    name = "webprint-client",
    version,
    about = "Onsite print client for webprint: fetches print jobs and prints them",
    long_about = "Onsite print client for webprint.\n\n\
                  It authenticates with the site token, waits for print jobs, and sends each \
                  one to the printer. The token identifies the site, so no site name is needed.\n\n\
                  If --token is not given, the client asks for it on start-up.\n\n\
                  A build can carry a default server address, so an organiser \
                  only has to run the program and type the token:\n    \
                  WEBPRINT_DEFAULT_SERVER=https://example.org/print \
                  cargo build --release -p webprint-client"
)]
struct Args {
    /// Server base URL, e.g. http://10.0.0.5:8080
    ///
    /// Optional when the binary was built with a server baked in; see
    /// DEFAULT_SERVER below.
    #[arg(short, long, env = "WEBPRINT_SERVER")]
    server: Option<String>,

    /// Site token. Asked for interactively when omitted.
    #[arg(short, long, env = "WEBPRINT_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Printer name; the system default is used when omitted.
    #[arg(short, long, env = "WEBPRINT_PRINTER")]
    printer: Option<String>,

    /// Printing backend: auto, lp, lpr, sumatra, acrobat, shell or dry-run.
    #[arg(long, env = "WEBPRINT_BACKEND", default_value = "auto")]
    backend: String,

    /// Custom print command, e.g. "lp -d {printer} {file}".
    #[arg(long, env = "WEBPRINT_PRINT_COMMAND")]
    print_command: Option<String>,

    /// Name reported to the server, shown in its stats.
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

    /// Handle at most one poll cycle, then exit. Useful for cron and tests.
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
    /// Print a test page to check the printing path.
    TestPage,
    /// Show the site's queue statistics and exit.
    Stats,
}

/// A server address compiled in at build time, for handing an organiser a
/// binary that only needs a token. Set `WEBPRINT_DEFAULT_SERVER` when
/// building; release builds leave it unset.
const BAKED_IN_SERVER: Option<&str> = option_env!("WEBPRINT_DEFAULT_SERVER");

static STOPPING: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
/// True while the status panel owns the terminal. Terminal control sequences
/// are only emitted then, so log mode stays clean for a log file.
static LIVE_UI: AtomicBool = AtomicBool::new(false);

fn main() {
    let args = Args::parse();
    let interactive = std::io::stdin().is_terminal();

    let result = match args.command {
        Some(Command::Printers) => list_printers(),
        Some(Command::TestPage) => test_page(&args),
        Some(Command::Stats) => show_stats(&args),
        Some(Command::Run) | None => run(args),
    };

    if let Err(error) = result {
        eprintln!("Error: {error:#}");
        // Started by double-clicking, the console closes the moment the
        // program exits, taking the message with it.
        if interactive {
            eprint!("\nPress Enter to close. ");
            let _ = std::io::stderr().flush();
            let mut discard = String::new();
            let _ = std::io::stdin().read_line(&mut discard);
        }
        std::process::exit(1);
    }
}

/// Resolves the printing backend and reports what it chose.
fn resolve_backend(args: &Args) -> Result<Backend> {
    let backend = printer::detect(Some(&args.backend), args.print_command.as_deref())?;
    if args.printer.is_some() && !backend.supports_printer_choice() {
        eprintln!(
            "warning: the {} backend cannot choose a printer, so --printer is ignored",
            backend.label()
        );
    }
    Ok(backend)
}

/// Asks for anything that was not passed on the command line.
fn resolve_credentials(args: &Args) -> Result<(String, String)> {
    let interactive = std::io::stdin().is_terminal();

    let server = match args
        .server
        .clone()
        .or_else(|| BAKED_IN_SERVER.map(str::to_string))
    {
        Some(s) => s,
        None if interactive => {
            print!("Server URL [http://localhost:8080]: ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let line = line.trim().to_string();
            if line.is_empty() {
                "http://localhost:8080".to_string()
            } else {
                line
            }
        }
        None => bail!(
            "no server given: pass --server, set WEBPRINT_SERVER, or build with \
             WEBPRINT_DEFAULT_SERVER set"
        ),
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

fn list_printers() -> Result<()> {
    let printers = printer::list_printers()?;
    if printers.is_empty() {
        println!("No printers found.");
        return Ok(());
    }
    println!("Printers:");
    for p in &printers {
        println!(
            "  {}{}{}",
            p.name,
            if p.is_default { "  (default)" } else { "" },
            p.driver
                .as_deref()
                .map(|d| format!("  [{d}]"))
                .unwrap_or_default()
        );
    }
    match printer::detect(Some("auto"), None) {
        Ok(backend) => println!("\nPrinting backend: {}", backend.label()),
        Err(e) => println!("\nNo printing backend available: {e}"),
    }
    Ok(())
}

fn show_stats(args: &Args) -> Result<()> {
    let (api, name) = connect(args)?;
    let stats = api.stats()?;

    let backend = printer::detect(Some(&args.backend), args.print_command.as_deref())
        .map(|b| b.label())
        .unwrap_or_else(|e| format!("unavailable ({e})"));
    let mut status = Status::new(api.base().to_string(), name, backend, args.printer.clone());
    status.tz_minutes = ui::parse_offset_minutes(stats.timezone.as_deref().unwrap_or("UTC"));
    status.mark_online();
    status.site = Some((stats.name.clone(), stats.site.clone()));
    status.recent = stats.recent.iter().take(10).cloned().collect();
    status.stats = Some(stats);

    println!("{}", ui::plain(&ui::render(&status, now_unix())));
    Ok(())
}

/// Renders a local test page and prints it, exercising the whole print path.
fn test_page(args: &Args) -> Result<()> {
    let backend = resolve_backend(args)?;
    let printer_name = args.printer.clone().or_else(printer::default_printer);
    println!(
        "Printing a test page to {} via {}...",
        printer_name
            .clone()
            .unwrap_or_else(|| "the default printer".into()),
        backend.label()
    );

    let content = format!(
        "webprint test page\n\n\
         This page was produced by webprint-client {}.\n\
         Host: {}\n\
         Backend: {}\n\
         Printer: {}\n\n\
         If you can read this, the printing path works:\n\
         the client can render a job, hand it to the printing subsystem, and\n\
         the printer can put it on paper.\n\n\
         Alignment check - the box below should have square corners and sit\n\
         inside an even 1.25 cm margin on A4:\n\n\
         +----------------------------------------------------------+\n\
         |                                                          |\n\
         |  Latin  ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrs   |\n\
         |  Cyrillic  АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ              |\n\
         |  Digits  0123456789  Symbols  !@#$%^&*(){{}}[]<>?/\\|~    |\n\
         |                                                          |\n\
         +----------------------------------------------------------+\n",
        env!("CARGO_PKG_VERSION"),
        client_name(args),
        backend.label(),
        printer_name
            .clone()
            .unwrap_or_else(|| "system default".into()),
    );

    let rendered = webprint_pdf::Renderer::new().render(&webprint_pdf::PrintJob {
        team: "webprint test page",
        site: "local",
        label: "test",
        submitted_at: &ui::format_time(now_unix(), 0),
        content: &content,
        language: Some("plain"),
    });

    let path = std::env::temp_dir().join("webprint-test-page.pdf");
    std::fs::write(&path, &rendered.bytes)
        .with_context(|| format!("writing {}", path.display()))?;

    printer::print_file(
        &backend,
        &path,
        printer_name.as_deref(),
        Duration::from_secs(args.print_timeout),
    )?;
    println!("Sent. The PDF is at {}", path.display());
    if backend.is_in_process() {
        // The spooler accepts the job either way; a host-based driver simply
        // discards it. Saying so here is the difference between a five-minute
        // fix and a mystery during the contest.
        println!(
            "\nIf no paper comes out, this printer cannot interpret a PDF by itself.\n\
             Install SumatraPDF next to this program, or re-run with\n\
             --backend sumatra (or --print-command) to rasterise first."
        );
    }
    Ok(())
}

fn run(args: Args) -> Result<()> {
    let backend = resolve_backend(&args)?;
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

    let printer_name = args.printer.clone().or_else(|| {
        if backend.supports_printer_choice() {
            printer::default_printer()
        } else {
            None
        }
    });

    let status = Arc::new(Mutex::new({
        let mut s = Status::new(
            api.base().to_string(),
            name.clone(),
            backend.label(),
            printer_name.clone(),
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
                .unwrap_or_else(|| "system default".into()),
            backend.label()
        );
    }

    let poll_wait = Duration::from_secs(args.poll_seconds);
    let print_timeout = Duration::from_secs(args.print_timeout);

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
                    &backend,
                    &job,
                    printer_name.as_deref(),
                    print_timeout,
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
#[allow(clippy::too_many_arguments)]
fn handle_job(
    api: &Api,
    backend: &Backend,
    job: &ClaimedJob,
    printer_name: Option<&str>,
    print_timeout: Duration,
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

        let path = std::env::temp_dir().join(format!("webprint-job-{}.pdf", job.id));
        std::fs::write(&path, &pdf)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;

        if let Some(dir) = keep {
            if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| {
                std::fs::write(dir.join(format!("webprint-job-{}.pdf", job.id)), &pdf)
            }) {
                eprintln!("warning: could not keep a copy in {}: {e}", dir.display());
            }
        }

        let print_result = printer::print_file(backend, &path, printer_name, print_timeout)
            .map_err(|e| e.to_string());
        // The spool file is no longer needed either way.
        let _ = std::fs::remove_file(&path);
        print_result
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
