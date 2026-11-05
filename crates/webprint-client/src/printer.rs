//! Sending a PDF to a physical printer.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::runtime::Runtime;
use universal_print::{Job, Printer, Sink};

pub use universal_print::DEFAULT_TIMEOUT;

const PDF: &str = "application/pdf";

const A4: &str = "iso_a4_210x297mm";

pub fn runtime() -> Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the runtime that talks to the printer")
}

/// Uses `--backend` to build a universal-print [`Sink`].
pub fn open(spec: &str, printer: Option<&str>, timeout: Duration) -> Result<Sink> {
    Sink::detect(Some(spec), printer, timeout).map_err(|e| anyhow!("{e}"))
}

/// Label for a status panel.
pub fn label(sink: &Sink) -> String {
    match sink {
        Sink::Ipp(ipp) => format!("IPP to {}", ipp.uri()),
        Sink::Local { backend, .. } => backend.label(),
    }
}

/// What to call the printer when none was named: an IPP URI carries its own
/// queue, so "system default" would be a plain untruth there.
pub fn unnamed_printer(sink: &Sink) -> &'static str {
    match sink {
        Sink::Ipp(_) => "network printer",
        Sink::Local { .. } => "system default",
    }
}

pub fn check(rt: &Runtime, sink: &Sink) -> Result<()> {
    rt.block_on(sink.check(PDF)).map_err(|e| anyhow!("{e}"))
}

/// Prints a document, blocking until the printer has taken it.
///
/// `title` is what the printer's queue shows, and `user` who the job is for,
/// where the backend can pass either on.
pub fn print(
    rt: &Runtime,
    sink: &Sink,
    document: &[u8],
    title: &str,
    user: Option<&str>,
) -> Result<()> {
    let job = Job {
        media: Some(A4),
        user,
        ..Job::new(document, PDF, title)
    };
    rt.block_on(sink.print(&job)).map_err(|e| anyhow!("{e}"))
}

/// Lists the printers the backend can reach. An IPP URI names its own queue,
/// so it has none to offer.
pub fn printers(rt: &Runtime, sink: &Sink) -> Result<Vec<Printer>> {
    rt.block_on(sink.printers()).map_err(|e| anyhow!("{e}"))
}

/// The printer a job goes to when none is named, if the backend knows it.
pub fn default_printer(sink: &Sink) -> Option<String> {
    match sink {
        Sink::Ipp(_) => None,
        Sink::Local { backend, .. } => backend.default_printer(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Parsing specs, running backends and timeouts are universal-print's and
    // tested there; this checks only what the client adds on top.
    #[test]
    fn the_status_panel_describes_the_destination() {
        let ipp = open("ipp://printer.lan/ipp/print", None, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(label(&ipp), "IPP to ipp://printer.lan/ipp/print");
        assert_eq!(unnamed_printer(&ipp), "network printer");
        assert_eq!(default_printer(&ipp), None);

        let local = open("dry-run", None, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(label(&local), "dry run");
        assert_eq!(unnamed_printer(&local), "system default");
    }
}
