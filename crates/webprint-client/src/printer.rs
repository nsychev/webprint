//! Sending a PDF to a physical printer, on Windows and on Unix.
//!
//! Unix has CUPS, which converts and prints PDFs for any printer it supports
//! (`lp`/`lpr`).
//!
//! On Windows the client talks to the spooler directly through the `printers`
//! crate, so nothing extra has to be installed beside it. The document is
//! spooled as-is, which every PostScript- or PDF-capable printer understands -
//! that covers the network laser printers venues normally use. A host-based
//! (GDI-only) printer cannot interpret a PDF itself; for those, point
//! `--backend` at SumatraPDF or Acrobat, which rasterise first. Printing a
//! test page once at setup settles which case you are in.
//!
//! `--print-command` overrides all of it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// How the client talks to the printing subsystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The platform's own spooler, with no helper application. Windows only;
    /// Unix uses CUPS through `lp`.
    Native,
    /// CUPS `lp`.
    Lp,
    /// BSD `lpr`.
    Lpr,
    /// SumatraPDF, the usual way to print PDFs silently on Windows.
    Sumatra(PathBuf),
    /// Adobe Acrobat / Reader in "print to printer" mode.
    Acrobat(PathBuf),
    /// Shell `Print` verb via PowerShell; uses the default PDF handler and
    /// cannot choose a printer.
    ShellPrint,
    /// A user-supplied command template.
    Command(String),
    /// Render to disk only; nothing is printed.
    DryRun,
}

impl Backend {
    /// A short name for the status display.
    pub fn label(&self) -> String {
        match self {
            Backend::Native => "Windows spooler (needs a PDF-capable printer)".into(),
            Backend::Lp => "lp (CUPS)".into(),
            Backend::Lpr => "lpr".into(),
            Backend::Sumatra(p) => format!("SumatraPDF ({})", p.display()),
            Backend::Acrobat(p) => format!("Acrobat ({})", p.display()),
            Backend::ShellPrint => "Windows print verb".into(),
            Backend::Command(t) => format!("command: {t}"),
            Backend::DryRun => "dry run (nothing is printed)".into(),
        }
    }

    /// Whether this backend can target a named printer.
    pub fn supports_printer_choice(&self) -> bool {
        !matches!(self, Backend::ShellPrint | Backend::DryRun)
    }

    /// Whether printing happens in-process rather than by running a command.
    pub fn is_in_process(&self) -> bool {
        matches!(self, Backend::Native)
    }
}

/// Chooses a backend, honouring an explicit choice.
pub fn detect(explicit: Option<&str>, print_command: Option<&str>) -> Result<Backend> {
    if let Some(template) = print_command {
        return Ok(Backend::Command(template.to_string()));
    }
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        None | Some("auto") => auto_detect(),
        Some("native") => {
            if cfg!(windows) {
                Ok(Backend::Native)
            } else {
                bail!("the native backend is Windows-only; on Unix use lp or lpr")
            }
        }
        Some("lp") => Ok(Backend::Lp),
        Some("lpr") => Ok(Backend::Lpr),
        Some("sumatra") => find_sumatra()
            .map(Backend::Sumatra)
            .ok_or_else(|| anyhow!("SumatraPDF was not found; pass --print-command instead")),
        Some("acrobat") => find_acrobat()
            .map(Backend::Acrobat)
            .ok_or_else(|| anyhow!("Acrobat was not found; pass --print-command instead")),
        Some("shell") | Some("powershell") => Ok(Backend::ShellPrint),
        Some("dry-run") | Some("none") => Ok(Backend::DryRun),
        Some(other) => bail!(
            "unknown backend {other:?}; use auto, native, lp, lpr, sumatra, acrobat, \
             shell or dry-run"
        ),
    }
}

#[cfg(unix)]
fn auto_detect() -> Result<Backend> {
    if which("lp").is_some() {
        return Ok(Backend::Lp);
    }
    if which("lpr").is_some() {
        return Ok(Backend::Lpr);
    }
    bail!("no printing command found: install CUPS (lp) or pass --print-command")
}

#[cfg(windows)]
fn auto_detect() -> Result<Backend> {
    // The spooler needs no helper application, so it is the default. A helper
    // is only better for printers that cannot interpret a PDF themselves, and
    // choosing one is then an explicit --backend.
    Ok(Backend::Native)
}

#[cfg(not(any(unix, windows)))]
fn auto_detect() -> Result<Backend> {
    bail!("unsupported platform: pass --print-command")
}

/// Looks for an executable on `PATH`.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.BAT;.CMD".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Finds SumatraPDF next to the client, on `PATH`, or where it installs.
pub fn find_sumatra() -> Option<PathBuf> {
    // Shipping SumatraPDF.exe beside the client is the easiest onsite setup.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["SumatraPDF.exe", "SumatraPDF-portable.exe"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    if let Some(found) = which("SumatraPDF") {
        return Some(found);
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let candidate = PathBuf::from(base)
                .join("SumatraPDF")
                .join("SumatraPDF.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Finds Acrobat Reader or Acrobat.
pub fn find_acrobat() -> Option<PathBuf> {
    for exe in ["AcroRd32", "Acrobat"] {
        if let Some(found) = which(exe) {
            return Some(found);
        }
    }
    let candidates = [
        r"Adobe\Acrobat Reader DC\Reader\AcroRd32.exe",
        r"Adobe\Reader 11.0\Reader\AcroRd32.exe",
        r"Adobe\Acrobat DC\Acrobat\Acrobat.exe",
    ];
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(var) {
            for rel in candidates {
                let candidate = PathBuf::from(&base).join(rel);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Expands `{file}` and `{printer}` in a command template.
pub fn expand_template(template: &str, file: &Path, printer: Option<&str>) -> String {
    template
        .replace("{file}", &file.display().to_string())
        .replace("{printer}", printer.unwrap_or(""))
}

/// Prints `file`, returning the command's own output on failure.
pub fn print_file(
    backend: &Backend,
    file: &Path,
    printer: Option<&str>,
    timeout: Duration,
) -> Result<()> {
    if backend.is_in_process() {
        return print_native(file, printer);
    }

    let mut command = build_command(backend, file, printer)?;
    let Some(mut command) = command.take() else {
        return Ok(()); // dry run
    };

    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("starting the print command for {}", file.display()))?;

    // A wedged print command must not stall the queue for everyone else.
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(status) => {
                let output = child.wait_with_output()?;
                if status.success() {
                    return Ok(());
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail: String = [stderr.trim(), stdout.trim()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" / ");
                bail!(
                    "print command failed ({}){}",
                    status
                        .code()
                        .map(|c| format!("exit {c}"))
                        .unwrap_or_else(|| "terminated".into()),
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", truncate(&detail, 300))
                    }
                );
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                bail!("print command timed out after {}s", timeout.as_secs());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Spools a document through the platform's own printing API.
#[cfg(windows)]
fn print_native(file: &Path, printer: Option<&str>) -> Result<()> {
    use printers::common::base::job::PrinterJobOptions;

    let printer = printer.filter(|p| !p.is_empty());
    let target = match printer {
        Some(name) => printers::get_printer_by_name(name).ok_or_else(|| {
            anyhow!("no printer named {name:?}; run `webprint-client printers` to list them")
        })?,
        None => printers::get_default_printer()
            .ok_or_else(|| anyhow!("no default printer is set on this machine; pass --printer"))?,
    };

    let path = file
        .to_str()
        .ok_or_else(|| anyhow!("the spool path is not valid UTF-8: {}", file.display()))?;

    target
        .print_file(
            path,
            PrinterJobOptions {
                name: Some("webprint"),
                ..PrinterJobOptions::none()
            },
        )
        .map_err(|e| anyhow!("the printer rejected the job: {}", e.message))?;
    Ok(())
}

#[cfg(not(windows))]
fn print_native(_file: &Path, _printer: Option<&str>) -> Result<()> {
    bail!("the native backend is Windows-only; on Unix use lp or lpr")
}

/// Builds the command for a backend. `None` means "nothing to run".
fn build_command(backend: &Backend, file: &Path, printer: Option<&str>) -> Result<Option<Command>> {
    let printer = printer.filter(|p| !p.is_empty());
    let cmd = match backend {
        Backend::DryRun => return Ok(None),
        // Handled in-process by `print_native`.
        Backend::Native => return Ok(None),
        Backend::Lp => {
            let mut c = Command::new("lp");
            if let Some(p) = printer {
                c.arg("-d").arg(p);
            }
            // Ask CUPS for A4 and wait for the job to be accepted.
            c.arg("-o").arg("media=A4").arg("--").arg(file);
            c
        }
        Backend::Lpr => {
            let mut c = Command::new("lpr");
            if let Some(p) = printer {
                c.arg("-P").arg(p);
            }
            c.arg(file);
            c
        }
        Backend::Sumatra(exe) => {
            let mut c = Command::new(exe);
            match printer {
                Some(p) => {
                    c.arg("-print-to").arg(p);
                }
                None => {
                    c.arg("-print-to-default");
                }
            }
            c.arg("-silent").arg("-exit-when-done").arg(file);
            c
        }
        Backend::Acrobat(exe) => {
            let mut c = Command::new(exe);
            // /t prints to a named printer, /p to the default one.
            match printer {
                Some(p) => {
                    c.arg("/t").arg(file).arg(p);
                }
                None => {
                    c.arg("/p").arg("/h").arg(file);
                }
            }
            c
        }
        Backend::ShellPrint => {
            let mut c = Command::new("powershell");
            c.arg("-NoProfile").arg("-NonInteractive").arg("-Command");
            c.arg(format!(
                "Start-Process -FilePath '{}' -Verb Print -PassThru | \
                 ForEach-Object {{ Start-Sleep -Seconds 5; $_ }} | Out-Null",
                file.display()
            ));
            c
        }
        Backend::Command(template) => {
            let line = expand_template(template, file, printer);
            if cfg!(windows) {
                let mut c = Command::new("cmd");
                c.arg("/C").arg(line);
                c
            } else {
                let mut c = Command::new("sh");
                c.arg("-c").arg(line);
                c
            }
        }
    };
    Ok(Some(cmd))
}

/// A printer known to the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub is_default: bool,
    /// Driver name, where the platform reports one. Worth seeing on Windows:
    /// a host-based driver cannot interpret a PDF itself.
    pub driver: Option<String>,
}

/// Lists printers, best effort.
pub fn list_printers() -> Result<Vec<Printer>> {
    if cfg!(windows) {
        list_printers_windows()
    } else {
        list_printers_unix()
    }
}

fn list_printers_unix() -> Result<Vec<Printer>> {
    let out = Command::new("lpstat")
        .arg("-e")
        .output()
        .context("running lpstat -e (is CUPS installed?)")?;
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let default = default_printer();
    Ok(names
        .into_iter()
        .map(|name| Printer {
            is_default: default.as_deref() == Some(name.as_str()),
            name,
            driver: None,
        })
        .collect())
}

#[cfg(windows)]
fn list_printers_windows() -> Result<Vec<Printer>> {
    // Windows can leave the "default" attribute set on more than one printer,
    // so take the system's answer as the single authority instead.
    let default = default_printer();
    Ok(printers::get_printers()
        .into_iter()
        .map(|p| Printer {
            is_default: default.as_deref() == Some(p.name.as_str()),
            name: p.name,
            driver: Some(p.driver_name),
        })
        .collect())
}

#[cfg(not(windows))]
fn list_printers_windows() -> Result<Vec<Printer>> {
    bail!("not running on Windows")
}

/// The system default printer, if one is set.
#[cfg(windows)]
pub fn default_printer() -> Option<String> {
    printers::get_default_printer().map(|p| p.name)
}

/// The system default printer, if one is set.
#[cfg(not(windows))]
pub fn default_printer() -> Option<String> {
    {
        let out = Command::new("lpstat").arg("-d").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        // "system default destination: NAME"
        text.split_once(':')
            .map(|(_, name)| name.trim().to_string())
            .filter(|n| !n.is_empty())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command template for the platform's shell: templates run through
    /// `sh -c` on Unix and `cmd /C` on Windows, which share no syntax.
    fn script(unix: &str, windows: &str) -> String {
        if cfg!(windows) {
            windows.to_string()
        } else {
            unix.to_string()
        }
    }

    #[test]
    fn the_native_backend_is_windows_only() {
        let chosen = detect(Some("native"), None);
        if cfg!(windows) {
            assert_eq!(chosen.unwrap(), Backend::Native);
        } else {
            let err = chosen.unwrap_err().to_string();
            assert!(err.contains("Windows-only"), "{err}");
            // And it refuses to print rather than doing something surprising.
            let err = print_file(
                &Backend::Native,
                Path::new("/tmp/job.pdf"),
                None,
                Duration::from_secs(1),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("Windows-only"), "{err}");
        }
    }

    #[test]
    fn auto_detection_picks_the_platform_default() {
        let backend = detect(Some("auto"), None);
        if cfg!(windows) {
            // No helper application needs installing.
            assert_eq!(backend.unwrap(), Backend::Native);
        } else {
            // CUPS, when it is there.
            assert!(matches!(
                backend,
                Ok(Backend::Lp) | Ok(Backend::Lpr) | Err(_)
            ));
        }
    }

    #[test]
    fn the_native_backend_prints_in_process_and_can_choose_a_printer() {
        assert!(Backend::Native.is_in_process());
        assert!(!Backend::Lp.is_in_process());
        assert!(Backend::Native.supports_printer_choice());
        assert!(Backend::Native.label().contains("PDF-capable"));
        // Nothing is spawned for it.
        assert!(
            build_command(&Backend::Native, Path::new("/tmp/j.pdf"), None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_explicit_backend_wins_over_detection() {
        assert_eq!(detect(Some("lp"), None).unwrap(), Backend::Lp);
        assert_eq!(detect(Some("LPR"), None).unwrap(), Backend::Lpr);
        assert_eq!(detect(Some("dry-run"), None).unwrap(), Backend::DryRun);
        assert_eq!(detect(Some("shell"), None).unwrap(), Backend::ShellPrint);
        assert!(detect(Some("nonsense"), None).is_err());
    }

    #[test]
    fn a_print_command_overrides_everything() {
        let backend = detect(Some("lp"), Some("mycmd {file}")).unwrap();
        assert_eq!(backend, Backend::Command("mycmd {file}".into()));
    }

    #[test]
    fn templates_expand_file_and_printer() {
        let file = Path::new("/tmp/job.pdf");
        assert_eq!(
            expand_template("lp -d {printer} {file}", file, Some("HP")),
            "lp -d HP /tmp/job.pdf"
        );
        // An absent printer leaves an empty slot rather than the literal token.
        assert_eq!(
            expand_template("print {printer} {file}", file, None),
            "print  /tmp/job.pdf"
        );
        assert_eq!(
            expand_template("print {file}", file, None),
            "print /tmp/job.pdf"
        );
    }

    #[test]
    fn backends_describe_themselves_and_their_capabilities() {
        assert!(Backend::Lp.label().contains("CUPS"));
        assert!(Backend::DryRun.label().contains("nothing"));
        assert!(Backend::Sumatra("/x/S.exe".into())
            .label()
            .contains("S.exe"));
        assert!(Backend::Lp.supports_printer_choice());
        assert!(Backend::Sumatra("/x".into()).supports_printer_choice());
        // The shell verb hands off to the default PDF application.
        assert!(!Backend::ShellPrint.supports_printer_choice());
        assert!(!Backend::DryRun.supports_printer_choice());
    }

    #[test]
    fn the_lp_command_targets_the_requested_printer() {
        let file = Path::new("/tmp/job.pdf");
        let cmd = build_command(&Backend::Lp, file, Some("HP"))
            .unwrap()
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "lp");
        assert!(args.windows(2).any(|w| w == ["-d", "HP"]), "{args:?}");
        assert!(args.contains(&"media=A4".to_string()), "{args:?}");
        assert!(args.last().unwrap().ends_with("job.pdf"));

        // Without a printer, lp uses the system default.
        let cmd = build_command(&Backend::Lp, file, None).unwrap().unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.contains(&"-d".to_string()), "{args:?}");

        // An empty printer name is treated as "unset".
        let cmd = build_command(&Backend::Lp, file, Some(""))
            .unwrap()
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.contains(&"-d".to_string()), "{args:?}");
    }

    #[test]
    fn the_sumatra_command_is_silent_and_exits() {
        let file = Path::new(r"C:\tmp\job.pdf");
        let backend = Backend::Sumatra(PathBuf::from(r"C:\tools\SumatraPDF.exe"));
        let cmd = build_command(&backend, file, Some("HP LaserJet"))
            .unwrap()
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2).any(|w| w == ["-print-to", "HP LaserJet"]),
            "{args:?}"
        );
        assert!(args.contains(&"-silent".to_string()), "{args:?}");
        assert!(args.contains(&"-exit-when-done".to_string()), "{args:?}");

        let cmd = build_command(&backend, file, None).unwrap().unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-print-to-default".to_string()), "{args:?}");
    }

    #[test]
    fn a_dry_run_builds_no_command_and_prints_nothing() {
        let file = Path::new("/tmp/job.pdf");
        assert!(build_command(&Backend::DryRun, file, None)
            .unwrap()
            .is_none());
        assert!(print_file(&Backend::DryRun, file, None, Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn a_failing_command_reports_its_output() {
        let file = Path::new("/tmp/job.pdf");
        let backend = Backend::Command(script(
            "echo some-failure >&2; exit 3",
            "echo some-failure 1>&2 && exit 3",
        ));
        let err = print_file(&backend, file, None, Duration::from_secs(10))
            .unwrap_err()
            .to_string();
        assert!(err.contains("exit 3"), "{err}");
        assert!(err.contains("some-failure"), "{err}");
    }

    #[test]
    fn a_successful_command_reports_success() {
        let file = Path::new("/tmp/job.pdf");
        let backend = Backend::Command(script("true", "exit 0"));
        assert!(print_file(&backend, file, None, Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn a_hanging_command_is_killed() {
        let file = Path::new("/tmp/job.pdf");
        // Both forms simply wait: the point is that the client gives up.
        let backend = Backend::Command(script("sleep 30", "ping -n 31 127.0.0.1 > nul"));
        let err = print_file(&backend, file, None, Duration::from_millis(300))
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn long_command_output_is_trimmed() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdef", 3), "abc...");
    }
}
