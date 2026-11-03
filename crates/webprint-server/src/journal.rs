//! Optional on-disk state, so a restart does not lose the queue.
//!
//! The queue lives in memory. Without this, restarting the server - a crash,
//! a deploy, a power cut mid-contest - loses every job that has not printed
//! yet, and teams find out by waiting at a printer that never produces
//! anything.
//!
//! The state is small: the submitted text of the jobs still in flight, plus
//! each site's counters. It is written as an append-only file of JSON lines,
//! one per event, which needs no database, keeps the server a single static
//! binary, and can be read by an operator with `tail`. The file is rewritten
//! as a snapshot of live state at start-up, so it does not grow across a
//! contest.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A change worth surviving a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A job was accepted. Carries everything needed to print it later.
    Submitted {
        site: String,
        id: u64,
        username: String,
        team: String,
        content: String,
        pages: usize,
        lines: usize,
        chars: usize,
        at: i64,
    },
    /// A client took the job. Recorded for the attempt count; a job that was
    /// printing when the server stopped goes back to the queue on restart,
    /// because its client is gone.
    Claimed { site: String, id: u64, at: i64 },
    /// The job reached a terminal state and no longer needs its text.
    Finished {
        site: String,
        id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        at: i64,
    },
    /// Absolute counters, written when the file is rewritten so that totals
    /// survive compaction.
    Counters {
        site: String,
        submitted: u64,
        processed: u64,
        failed: u64,
        pages_printed: u64,
    },
}

/// Appends events to a file, or does nothing when persistence is off.
pub struct Journal {
    path: Option<PathBuf>,
    file: Mutex<Option<File>>,
}

impl Journal {
    /// A journal that records nothing.
    pub fn disabled() -> Self {
        Self {
            path: None,
            file: Mutex::new(None),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Opens `path`, returning the events recorded so far. A missing file
    /// simply means there is nothing to restore.
    pub fn open(path: &Path) -> Result<(Self, Vec<Event>)> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let events = Self::read(path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok((
            Self {
                path: Some(path.to_path_buf()),
                file: Mutex::new(Some(file)),
            },
            events,
        ))
    }

    /// Reads every complete event from a file.
    fn read(path: &Path) -> Result<Vec<Event>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };

        let mut events = Vec::new();
        for (number, line) in BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(line) => line,
                // A torn write at the end of the file: everything before it
                // is still good.
                Err(e) => {
                    tracing::warn!(line = number + 1, error = %e, "ignoring unreadable state line");
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(&line) {
                Ok(event) => events.push(event),
                Err(e) => {
                    tracing::warn!(
                        line = number + 1,
                        error = %e,
                        "ignoring unreadable state line"
                    );
                }
            }
        }
        Ok(events)
    }

    /// Records one event. A failure is logged and swallowed: losing the
    /// journal is better than refusing to print.
    pub fn append(&self, event: &Event) {
        let mut guard = self.file.lock().expect("journal mutex poisoned");
        let Some(file) = guard.as_mut() else {
            return;
        };
        if let Err(e) = Self::write_line(file, event) {
            tracing::error!(error = %e, "could not record state; continuing without it");
        }
    }

    fn write_line(file: &mut File, event: &Event) -> std::io::Result<()> {
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        // Flush to the device: a power cut is exactly the case this is for.
        file.sync_data()
    }

    /// Replaces the file with `events`, through a temporary file and a rename
    /// so an interrupted rewrite cannot leave a half-written state.
    pub fn compact(&self, events: &[Event]) -> Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let mut guard = self.file.lock().expect("journal mutex poisoned");

        let temporary = path.with_extension("tmp");
        {
            let mut file = File::create(&temporary)
                .with_context(|| format!("creating {}", temporary.display()))?;
            for event in events {
                Self::write_line(&mut file, event)
                    .with_context(|| format!("writing {}", temporary.display()))?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("replacing {}", path.display()))?;

        *guard = Some(
            OpenOptions::new()
                .append(true)
                .open(&path)
                .with_context(|| format!("reopening {}", path.display()))?,
        );
        Ok(())
    }
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal").field("path", &self.path).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("webprint-journal-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn submitted(id: u64, content: &str) -> Event {
        Event::Submitted {
            site: "main".into(),
            id,
            username: "t1".into(),
            team: "Team One".into(),
            content: content.into(),
            pages: 1,
            lines: 2,
            chars: content.len(),
            at: 1_793_707_200,
        }
    }

    #[test]
    fn a_disabled_journal_records_nothing() {
        let journal = Journal::disabled();
        assert!(!journal.is_enabled());
        journal.append(&submitted(1, "x"));
        assert!(journal.compact(&[submitted(1, "x")]).is_ok());
    }

    #[test]
    fn events_survive_a_round_trip() {
        let path = temp_dir("roundtrip").join("state.jsonl");
        let (journal, events) = Journal::open(&path).unwrap();
        assert!(events.is_empty(), "a missing file restores nothing");
        assert!(journal.is_enabled());

        let written = vec![
            submitted(1, "fn main() {}\n"),
            submitted(2, "Привет, мир!\n"),
            Event::Claimed {
                site: "main".into(),
                id: 1,
                at: 5,
            },
            Event::Finished {
                site: "main".into(),
                id: 1,
                ok: true,
                error: None,
                at: 6,
            },
        ];
        for event in &written {
            journal.append(event);
        }
        drop(journal);

        let (_, read_back) = Journal::open(&path).unwrap();
        assert_eq!(read_back, written);
    }

    #[test]
    fn a_torn_last_line_does_not_lose_the_rest() {
        let path = temp_dir("torn").join("state.jsonl");
        let (journal, _) = Journal::open(&path).unwrap();
        journal.append(&submitted(1, "first"));
        journal.append(&submitted(2, "second"));
        drop(journal);

        // Simulate a power cut in the middle of a write.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"event\":\"submitted\",\"site\":\"ma");
        std::fs::write(&path, text).unwrap();

        let (_, events) = Journal::open(&path).unwrap();
        assert_eq!(events.len(), 2, "the complete lines must survive");
        assert_eq!(events[0], submitted(1, "first"));
    }

    #[test]
    fn rubbish_lines_are_skipped() {
        let path = temp_dir("rubbish").join("state.jsonl");
        std::fs::write(
            &path,
            "not json\n\n{\"event\":\"unknown_kind\"}\n{\"event\":\"claimed\",\"site\":\"main\",\"id\":7,\"at\":1}\n",
        )
        .unwrap();
        let (_, events) = Journal::open(&path).unwrap();
        assert_eq!(
            events,
            vec![Event::Claimed {
                site: "main".into(),
                id: 7,
                at: 1
            }]
        );
    }

    #[test]
    fn compaction_replaces_the_file_and_keeps_appending() {
        let path = temp_dir("compact").join("state.jsonl");
        let (journal, _) = Journal::open(&path).unwrap();
        for id in 1..=20 {
            journal.append(&submitted(id, "x"));
        }

        journal.compact(&[submitted(99, "kept")]).unwrap();
        journal.append(&Event::Claimed {
            site: "main".into(),
            id: 99,
            at: 3,
        });
        drop(journal);

        let (_, events) = Journal::open(&path).unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0], submitted(99, "kept"));
        // No leftovers from the interrupted rewrite.
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn the_directory_is_created_if_it_is_missing() {
        let path = temp_dir("nested").join("deeper").join("state.jsonl");
        let (journal, _) = Journal::open(&path).unwrap();
        journal.append(&submitted(1, "x"));
        assert!(path.exists());
    }
}
