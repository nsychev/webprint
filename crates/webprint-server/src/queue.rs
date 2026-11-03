//! In-memory print job queues, one per site.
//!
//! A job moves `Queued -> Printing -> Printed | Failed`. The onsite client
//! claims a job, fetches its PDF, prints it and confirms the outcome. A claim
//! carries a lease: if the client dies mid-job the lease expires and the job
//! goes back to the queue, up to [`Limits::max_attempts`] times, so a crashed
//! or unplugged client cannot silently swallow a team's printout.
//!
//! A queued job holds the submitted text, not a rendered document: the PDF is
//! built when the print client asks for it. Every document embeds the fonts,
//! so a rendered job costs a quarter of a megabyte whatever it says, while
//! its text is a few kilobytes. The text is dropped once the job finishes.
//!
//! Several clients may share one site token. They then act as workers on one
//! queue: each job goes to whichever claims it first, so jobs are spread
//! across the printers rather than duplicated onto all of them. Every client
//! that polls is recorded, so a forgotten one quietly taking jobs shows up in
//! the stats instead of being a mystery.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::Notify;

use crate::config::{Config, Limits};
use crate::journal::{Event, Journal};
use crate::util::now_unix;

pub type JobId = u64;

/// Lifecycle state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting for a print client to claim it.
    Queued,
    /// Claimed by a client and being printed.
    Printing,
    /// The client confirmed a successful print.
    Printed,
    /// Permanently failed (client reported an error, or too many lost leases).
    Failed,
}

impl JobState {
    pub fn label(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Printing => "printing",
            JobState::Printed => "printed",
            JobState::Failed => "failed",
        }
    }
}

/// Metadata produced while rendering a job.
#[derive(Debug, Clone, Copy)]
pub struct JobMeta {
    pub pages: usize,
    pub lines: usize,
    pub chars: usize,
}

#[derive(Debug, Clone)]
struct Job {
    id: JobId,
    username: String,
    team_name: String,
    meta: JobMeta,
    created_at: i64,
    state: JobState,
    attempts: u32,
    claimed_by: Option<String>,
    claimed_at: Option<i64>,
    finished_at: Option<i64>,
    error: Option<String>,
    /// The submitted text, kept until the job reaches a terminal state. The
    /// document is rendered from it on demand.
    content: Option<Arc<str>>,
}

/// Everything needed to build a job's document.
#[derive(Debug, Clone)]
pub struct JobSource {
    pub team: String,
    pub created_at: i64,
    pub content: Arc<str>,
}

/// A job as shown to teams and operators.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JobView {
    pub id: JobId,
    pub state: JobState,
    pub team: String,
    pub username: String,
    pub pages: usize,
    pub lines: usize,
    pub chars: usize,
    pub created_at: i64,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Job {
    fn view(&self) -> JobView {
        JobView {
            id: self.id,
            state: self.state,
            team: self.team_name.clone(),
            username: self.username.clone(),
            pages: self.meta.pages,
            lines: self.meta.lines,
            chars: self.meta.chars,
            created_at: self.created_at,
            attempts: self.attempts,
            claimed_by: self.claimed_by.clone(),
            finished_at: self.finished_at,
            error: self.error.clone(),
        }
    }
}

/// A job handed to a print client.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimedJob {
    pub id: JobId,
    pub team: String,
    /// Login of the team that submitted the job, shown in the client's status.
    pub username: String,
    pub site: String,
    pub pages: usize,
    pub lines: usize,
    pub created_at: i64,
    pub attempt: u32,
    /// Where to fetch the PDF, relative to the server root.
    pub pdf_url: String,
}

/// Why a submission was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    UnknownSite,
    /// The team already has `limit` jobs waiting.
    TooManyQueued {
        limit: usize,
    },
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::UnknownSite => write!(f, "unknown site"),
            SubmitError::TooManyQueued { limit } => write!(
                f,
                "you already have {limit} job(s) waiting to print; wait for them to finish"
            ),
        }
    }
}

/// Why a completion report was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompleteError {
    UnknownSite,
    UnknownJob,
    /// The job is not currently claimed (already finished, or requeued after
    /// its lease expired).
    NotClaimed(JobState),
}

impl std::fmt::Display for CompleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompleteError::UnknownSite => write!(f, "unknown site"),
            CompleteError::UnknownJob => write!(f, "unknown job"),
            CompleteError::NotClaimed(state) => {
                write!(f, "job is not being printed (state: {})", state.label())
            }
        }
    }
}

/// Counters and gauges for one site.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SiteStats {
    pub site: String,
    pub name: String,
    /// Jobs waiting to be claimed.
    pub queued: usize,
    /// Jobs claimed and currently printing.
    pub printing: usize,
    /// Jobs printed successfully since start-up.
    pub processed: u64,
    /// Jobs that permanently failed since start-up.
    pub failed: u64,
    /// Jobs accepted since start-up.
    pub submitted: u64,
    /// Pages printed successfully since start-up.
    pub pages_printed: u64,
    pub teams: usize,
    /// Age of the oldest queued job, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_queued_age: Option<i64>,
    /// Every print client that has polled this site, most recent first.
    /// More than one means they share the queue as workers.
    pub clients: Vec<ClientStatus>,
    /// Per-team submission counts, busiest first.
    pub per_team: Vec<TeamStats>,
}

/// A print client that has polled this site.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClientStatus {
    pub name: String,
    pub last_seen: i64,
    /// Jobs this client has taken since the server started.
    pub claimed: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeamStats {
    pub username: String,
    pub team: String,
    pub submitted: u64,
    pub printed: u64,
    pub failed: u64,
    pub queued: usize,
}

#[derive(Debug, Default)]
struct SiteQueue {
    waiting: VecDeque<JobId>,
    jobs: HashMap<JobId, Job>,
    /// Finished job ids in completion order, used to trim history.
    finished: VecDeque<JobId>,
    submitted: u64,
    processed: u64,
    failed: u64,
    pages_printed: u64,
    clients: HashMap<String, ClientRecord>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ClientRecord {
    last_seen: i64,
    claimed: u64,
}

struct SiteState {
    name: String,
    teams: usize,
    queue: Mutex<SiteQueue>,
    notify: Notify,
}

/// All print queues.
pub struct Store {
    limits: Limits,
    next_id: AtomicU64,
    sites: HashMap<String, SiteState>,
    journal: Journal,
}

impl Store {
    pub fn new(config: &Config) -> Self {
        let sites = config
            .sites
            .iter()
            .map(|(slug, site)| {
                (
                    slug.clone(),
                    SiteState {
                        name: site.display_name(slug),
                        // One login is one team, however many passwords it
                        // has listed (practice and contest, say).
                        teams: site
                            .teams
                            .iter()
                            .map(|t| t.username.as_str())
                            .collect::<std::collections::BTreeSet<_>>()
                            .len(),
                        queue: Mutex::new(SiteQueue::default()),
                        notify: Notify::new(),
                    },
                )
            })
            .collect();
        Self {
            limits: config.limits.clone(),
            next_id: AtomicU64::new(1),
            sites,
            journal: Journal::disabled(),
        }
    }

    /// Records changes to `journal`, and restores whatever it already holds.
    ///
    /// A job that was being printed when the server stopped goes back to the
    /// queue: its client is gone, and printing a job twice is better than a
    /// team never getting it.
    pub fn with_journal(mut self, journal: Journal, events: Vec<Event>) -> Self {
        let mut restored = 0usize;
        let mut highest = 0u64;

        for event in events {
            match event {
                Event::Submitted {
                    site,
                    id,
                    username,
                    team,
                    content,
                    pages,
                    lines,
                    chars,
                    at,
                } => {
                    highest = highest.max(id);
                    let Some(state) = self.sites.get(&site) else {
                        // The site is no longer configured.
                        continue;
                    };
                    let mut q = state.queue.lock().expect("queue mutex poisoned");
                    q.jobs.insert(
                        id,
                        Job {
                            id,
                            username,
                            team_name: team,
                            meta: JobMeta {
                                pages,
                                lines,
                                chars,
                            },
                            created_at: at,
                            state: JobState::Queued,
                            attempts: 0,
                            claimed_by: None,
                            claimed_at: None,
                            finished_at: None,
                            error: None,
                            content: Some(Arc::from(content)),
                        },
                    );
                    q.waiting.push_back(id);
                    q.submitted += 1;
                    restored += 1;
                }
                Event::Claimed { site, id, .. } => {
                    if let Some(state) = self.sites.get(&site) {
                        let mut q = state.queue.lock().expect("queue mutex poisoned");
                        if let Some(job) = q.jobs.get_mut(&id) {
                            job.attempts += 1;
                        }
                    }
                }
                Event::Finished { site, id, ok, .. } => {
                    let Some(state) = self.sites.get(&site) else {
                        continue;
                    };
                    let mut q = state.queue.lock().expect("queue mutex poisoned");
                    if let Some(job) = q.jobs.remove(&id) {
                        q.waiting.retain(|waiting| *waiting != id);
                        restored = restored.saturating_sub(1);
                        if ok {
                            q.processed += 1;
                            q.pages_printed += job.meta.pages as u64;
                        } else {
                            q.failed += 1;
                        }
                    }
                }
                Event::Counters {
                    site,
                    submitted,
                    processed,
                    failed,
                    pages_printed,
                } => {
                    if let Some(state) = self.sites.get(&site) {
                        let mut q = state.queue.lock().expect("queue mutex poisoned");
                        q.submitted = submitted;
                        q.processed = processed;
                        q.failed = failed;
                        q.pages_printed = pages_printed;
                    }
                }
            }
        }

        self.next_id = AtomicU64::new(highest + 1);
        self.journal = journal;

        if self.journal.is_enabled() {
            if restored > 0 {
                tracing::info!(jobs = restored, "restored queued jobs from the state file");
            }
            // Start the file afresh from what is actually live, so it does
            // not grow across restarts.
            if let Err(e) = self.journal.compact(&self.snapshot()) {
                tracing::error!(error = %e, "could not rewrite the state file");
            }
        }
        self
    }

    /// The events that reproduce the current state.
    fn snapshot(&self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut slugs: Vec<&String> = self.sites.keys().collect();
        slugs.sort();

        for slug in slugs {
            let state = &self.sites[slug];
            let q = state.queue.lock().expect("queue mutex poisoned");
            let mut live: Vec<&Job> = q
                .jobs
                .values()
                .filter(|j| matches!(j.state, JobState::Queued | JobState::Printing))
                .collect();
            live.sort_by_key(|j| j.id);

            for job in live {
                let Some(content) = job.content.as_ref() else {
                    continue;
                };
                events.push(Event::Submitted {
                    site: slug.clone(),
                    id: job.id,
                    username: job.username.clone(),
                    team: job.team_name.clone(),
                    content: content.to_string(),
                    pages: job.meta.pages,
                    lines: job.meta.lines,
                    chars: job.meta.chars,
                    at: job.created_at,
                });
            }
            // After the jobs, so it overrides the counts they just bumped.
            events.push(Event::Counters {
                site: slug.clone(),
                submitted: q.submitted,
                processed: q.processed,
                failed: q.failed,
                pages_printed: q.pages_printed,
            });
        }
        events
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    fn site(&self, slug: &str) -> Option<&SiteState> {
        self.sites.get(slug)
    }

    /// Checks whether `username` may submit another job right now. Called
    /// before rendering so an over-quota team does not cost us a render.
    pub fn check_capacity(&self, slug: &str, username: &str) -> Result<(), SubmitError> {
        let site = self.site(slug).ok_or(SubmitError::UnknownSite)?;
        let q = site.queue.lock().expect("queue mutex poisoned");
        if Self::pending_for(&q, username) >= self.limits.max_queued_per_team {
            return Err(SubmitError::TooManyQueued {
                limit: self.limits.max_queued_per_team,
            });
        }
        Ok(())
    }

    fn pending_for(q: &SiteQueue, username: &str) -> usize {
        q.jobs
            .values()
            .filter(|j| {
                j.username == username && matches!(j.state, JobState::Queued | JobState::Printing)
            })
            .count()
    }

    /// Reserves the next job id.
    ///
    /// The id is printed in the PDF header, so it has to be known before the
    /// document is rendered; [`Store::submit`] then enqueues under that id.
    /// A render that fails simply leaves a gap in the numbering.
    pub fn reserve_id(&self) -> JobId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Enqueues a rendered job under a previously reserved id.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &self,
        slug: &str,
        id: JobId,
        username: &str,
        team_name: &str,
        content: Arc<str>,
        meta: JobMeta,
        now: i64,
    ) -> Result<JobView, SubmitError> {
        let site = self.site(slug).ok_or(SubmitError::UnknownSite)?;
        let mut q = site.queue.lock().expect("queue mutex poisoned");

        if Self::pending_for(&q, username) >= self.limits.max_queued_per_team {
            return Err(SubmitError::TooManyQueued {
                limit: self.limits.max_queued_per_team,
            });
        }

        let recorded = content.to_string();
        let job = Job {
            id,
            username: username.to_string(),
            team_name: team_name.to_string(),
            meta,
            created_at: now,
            state: JobState::Queued,
            attempts: 0,
            claimed_by: None,
            claimed_at: None,
            finished_at: None,
            error: None,
            content: Some(content),
        };
        let view = job.view();
        q.jobs.insert(id, job);
        q.waiting.push_back(id);
        q.submitted += 1;
        drop(q);

        self.journal.append(&Event::Submitted {
            site: slug.to_string(),
            id,
            username: username.to_string(),
            team: team_name.to_string(),
            content: recorded,
            pages: meta.pages,
            lines: meta.lines,
            chars: meta.chars,
            at: now,
        });

        // Wake any client currently long-polling this site.
        site.notify.notify_waiters();
        Ok(view)
    }

    /// Records that a client polled, without handing out work.
    pub fn touch_client(&self, slug: &str, client: &str, now: i64) {
        if let Some(site) = self.site(slug) {
            let mut q = site.queue.lock().expect("queue mutex poisoned");
            Self::see_client(&mut q, client, now);
        }
    }

    /// Notes that `client` is alive, forgetting long-silent ones so a client
    /// that reports a fresh name every poll cannot grow the map without end.
    fn see_client(q: &mut SiteQueue, client: &str, now: i64) {
        let entry = q.clients.entry(client.to_string()).or_default();
        entry.last_seen = now;

        const MAX_TRACKED: usize = 16;
        if q.clients.len() > MAX_TRACKED {
            if let Some(oldest) = q
                .clients
                .iter()
                .min_by_key(|(name, record)| (record.last_seen, (*name).clone()))
                .map(|(name, _)| name.clone())
            {
                if oldest != client {
                    q.clients.remove(&oldest);
                }
            }
        }
    }

    /// Hands the next queued job to a client, if any.
    pub fn claim(&self, slug: &str, client: &str, now: i64) -> Option<ClaimedJob> {
        let site = self.site(slug)?;
        let mut q = site.queue.lock().expect("queue mutex poisoned");
        Self::see_client(&mut q, client, now);

        while let Some(id) = q.waiting.pop_front() {
            // The job borrow has to end before the client counter is touched.
            let claimed = {
                let Some(job) = q.jobs.get_mut(&id) else {
                    continue;
                };
                if job.state != JobState::Queued {
                    continue;
                }
                job.state = JobState::Printing;
                job.attempts += 1;
                job.claimed_by = Some(client.to_string());
                job.claimed_at = Some(now);
                ClaimedJob {
                    id,
                    team: job.team_name.clone(),
                    username: job.username.clone(),
                    site: slug.to_string(),
                    pages: job.meta.pages,
                    lines: job.meta.lines,
                    created_at: job.created_at,
                    attempt: job.attempts,
                    pdf_url: format!("/api/v1/jobs/{id}/pdf"),
                }
            };
            if let Some(record) = q.clients.get_mut(client) {
                record.claimed += 1;
            }
            drop(q);
            self.journal.append(&Event::Claimed {
                site: slug.to_string(),
                id,
                at: now,
            });
            return Some(claimed);
        }
        None
    }

    /// Waits up to `wait` for a job to become available, then claims it.
    pub async fn claim_wait(
        &self,
        slug: &str,
        client: &str,
        wait: std::time::Duration,
    ) -> Option<ClaimedJob> {
        let site = self.site(slug)?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            // Register interest *before* checking the queue so a job submitted
            // in between still wakes us.
            let notified = site.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if let Some(job) = self.claim(slug, client, now_unix()) {
                return Some(job);
            }
            tokio::select! {
                _ = notified => continue,
                _ = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }

    /// What a job needs to be rendered, while it is still in flight.
    pub fn source(&self, slug: &str, id: JobId) -> Option<JobSource> {
        let site = self.site(slug)?;
        let q = site.queue.lock().expect("queue mutex poisoned");
        let job = q.jobs.get(&id)?;
        Some(JobSource {
            team: job.team_name.clone(),
            created_at: job.created_at,
            content: job.content.clone()?,
        })
    }

    /// Records the outcome reported by a client.
    pub fn complete(
        &self,
        slug: &str,
        id: JobId,
        ok: bool,
        error: Option<String>,
        now: i64,
    ) -> Result<JobView, CompleteError> {
        let site = self.site(slug).ok_or(CompleteError::UnknownSite)?;
        let mut q = site.queue.lock().expect("queue mutex poisoned");

        let job = q.jobs.get_mut(&id).ok_or(CompleteError::UnknownJob)?;
        if job.state != JobState::Printing {
            return Err(CompleteError::NotClaimed(job.state));
        }

        job.state = if ok {
            JobState::Printed
        } else {
            JobState::Failed
        };
        job.finished_at = Some(now);
        job.error = error.filter(|_| !ok);
        // The text is no longer needed; releasing it keeps memory flat.
        job.content = None;
        let pages = job.meta.pages as u64;
        let view = job.view();

        if ok {
            q.processed += 1;
            q.pages_printed += pages;
        } else {
            q.failed += 1;
        }
        q.finished.push_back(id);
        self.trim(&mut q);
        drop(q);

        self.journal.append(&Event::Finished {
            site: slug.to_string(),
            id,
            ok,
            error: view.error.clone(),
            at: now,
        });
        Ok(view)
    }

    /// Requeues jobs whose lease expired, failing those out of attempts.
    /// Returns the number of jobs touched.
    pub fn sweep(&self, now: i64) -> usize {
        let lease = self.limits.lease_seconds as i64;
        let mut touched = 0;
        for (slug, site) in &self.sites {
            let mut q = site.queue.lock().expect("queue mutex poisoned");
            let stale: Vec<JobId> = q
                .jobs
                .values()
                .filter(|j| {
                    j.state == JobState::Printing && j.claimed_at.is_some_and(|at| now - at > lease)
                })
                .map(|j| j.id)
                .collect();

            let mut requeued = Vec::new();
            let mut failed_now: Vec<(JobId, Option<String>)> = Vec::new();
            for id in stale {
                let max_attempts = self.limits.max_attempts;
                let job = q.jobs.get_mut(&id).expect("id came from this map");
                touched += 1;
                if job.attempts >= max_attempts {
                    job.state = JobState::Failed;
                    job.finished_at = Some(now);
                    job.error = Some(format!(
                        "print client did not confirm after {} attempt(s)",
                        job.attempts
                    ));
                    job.content = None;
                    let reason = job.error.clone();
                    q.failed += 1;
                    q.finished.push_back(id);
                    failed_now.push((id, reason));
                    tracing::warn!(site = slug, job = id, "job failed: lease expired");
                } else {
                    job.state = JobState::Queued;
                    job.claimed_by = None;
                    job.claimed_at = None;
                    requeued.push(id);
                    tracing::warn!(site = slug, job = id, "lease expired, requeueing");
                }
            }
            // Requeue at the back so one unprintable job cannot block the head
            // of the queue for everyone else.
            for id in requeued {
                q.waiting.push_back(id);
            }
            if touched > 0 {
                self.trim(&mut q);
            }
            drop(q);

            for (id, error) in failed_now {
                self.journal.append(&Event::Finished {
                    site: slug.to_string(),
                    id,
                    ok: false,
                    error,
                    at: now,
                });
            }
            site.notify.notify_waiters();
        }
        touched
    }

    /// Drops the oldest finished jobs beyond the history limit.
    fn trim(&self, q: &mut SiteQueue) {
        while q.finished.len() > self.limits.job_history {
            if let Some(id) = q.finished.pop_front() {
                q.jobs.remove(&id);
            }
        }
    }

    /// Most recent jobs for one team, newest first.
    pub fn team_jobs(&self, slug: &str, username: &str, limit: usize) -> Vec<JobView> {
        let Some(site) = self.site(slug) else {
            return Vec::new();
        };
        let q = site.queue.lock().expect("queue mutex poisoned");
        let mut jobs: Vec<JobView> = q
            .jobs
            .values()
            .filter(|j| j.username == username)
            .map(Job::view)
            .collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.id));
        jobs.truncate(limit);
        jobs
    }

    /// Most recent jobs across a site, newest first.
    pub fn recent_jobs(&self, slug: &str, limit: usize) -> Vec<JobView> {
        let Some(site) = self.site(slug) else {
            return Vec::new();
        };
        let q = site.queue.lock().expect("queue mutex poisoned");
        let mut jobs: Vec<JobView> = q.jobs.values().map(Job::view).collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.id));
        jobs.truncate(limit);
        jobs
    }

    /// Statistics for one site.
    pub fn stats(&self, slug: &str, now: i64) -> Option<SiteStats> {
        let site = self.site(slug)?;
        let q = site.queue.lock().expect("queue mutex poisoned");

        let queued = q
            .jobs
            .values()
            .filter(|j| j.state == JobState::Queued)
            .count();
        let printing = q
            .jobs
            .values()
            .filter(|j| j.state == JobState::Printing)
            .count();
        let oldest_queued_age = q
            .jobs
            .values()
            .filter(|j| j.state == JobState::Queued)
            .map(|j| now - j.created_at)
            .max();

        let mut per_team: HashMap<&str, TeamStats> = HashMap::new();
        for job in q.jobs.values() {
            let entry = per_team
                .entry(job.username.as_str())
                .or_insert_with(|| TeamStats {
                    username: job.username.clone(),
                    team: job.team_name.clone(),
                    submitted: 0,
                    printed: 0,
                    failed: 0,
                    queued: 0,
                });
            entry.submitted += 1;
            match job.state {
                JobState::Printed => entry.printed += 1,
                JobState::Failed => entry.failed += 1,
                JobState::Queued | JobState::Printing => entry.queued += 1,
            }
        }
        let mut clients: Vec<ClientStatus> = q
            .clients
            .iter()
            .map(|(name, record)| ClientStatus {
                name: name.clone(),
                last_seen: record.last_seen,
                claimed: record.claimed,
            })
            .collect();
        clients.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.name.cmp(&b.name)));

        let mut per_team: Vec<TeamStats> = per_team.into_values().collect();
        per_team.sort_by(|a, b| {
            b.submitted
                .cmp(&a.submitted)
                .then_with(|| a.username.cmp(&b.username))
        });

        Some(SiteStats {
            site: slug.to_string(),
            name: site.name.clone(),
            queued,
            printing,
            processed: q.processed,
            failed: q.failed,
            submitted: q.submitted,
            pages_printed: q.pages_printed,
            teams: site.teams,
            oldest_queued_age,
            clients,
            per_team,
        })
    }

    /// Statistics for every site, ordered by slug.
    pub fn all_stats(&self, now: i64) -> Vec<SiteStats> {
        let mut slugs: Vec<&String> = self.sites.keys().collect();
        slugs.sort();
        slugs
            .into_iter()
            .filter_map(|s| self.stats(s, now))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_762_000_000;

    fn config(yaml_extra: &str) -> Config {
        let yaml = format!(
            "{yaml_extra}sites:\n  main:\n    name: Main\n    token: tok\n    teams:\n      \
             - {{username: t1, password: p, name: Team One}}\n      \
             - {{username: t1, password: p2, name: Team One}}\n      \
             - {{username: t2, password: p, name: Team Two}}\n  annex:\n    token: tok2\n    \
             teams:\n      - {{username: a1, password: p, name: Annex One}}\n"
        );
        Config::parse_str(&yaml).unwrap()
    }

    fn store(extra: &str) -> Store {
        Store::new(&config(extra))
    }

    fn meta() -> JobMeta {
        JobMeta {
            pages: 2,
            lines: 40,
            chars: 400,
        }
    }

    fn submit(s: &Store, site: &str, user: &str) -> JobView {
        let id = s.reserve_id();
        s.submit(site, id, user, "Team One", "hello".into(), meta(), NOW)
            .unwrap()
    }

    #[test]
    fn a_job_runs_queued_printing_printed() {
        let s = store("");
        let job = submit(&s, "main", "t1");
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.id, 1);

        let stats = s.stats("main", NOW).unwrap();
        assert_eq!((stats.queued, stats.printing, stats.processed), (1, 0, 0));
        assert_eq!(stats.submitted, 1);
        assert_eq!(stats.name, "Main");
        // t1 is listed twice, with a practice and a contest password, but it
        // is one team.
        assert_eq!(stats.teams, 2);

        let claimed = s.claim("main", "client-a", NOW + 1).expect("a job");
        assert_eq!(claimed.id, 1);
        assert_eq!(claimed.team, "Team One");
        assert_eq!(claimed.username, "t1");
        assert_eq!(claimed.attempt, 1);
        assert_eq!(claimed.pdf_url, "/api/v1/jobs/1/pdf");
        let source = s.source("main", 1).expect("the text is kept for rendering");
        assert_eq!(&*source.content, "hello");
        assert_eq!(source.team, "Team One");

        let stats = s.stats("main", NOW).unwrap();
        assert_eq!((stats.queued, stats.printing), (0, 1));
        assert_eq!(stats.clients.len(), 1);
        assert_eq!(stats.clients[0].name, "client-a");
        assert_eq!(stats.clients[0].last_seen, NOW + 1);
        assert_eq!(stats.clients[0].claimed, 1);

        let done = s.complete("main", 1, true, None, NOW + 5).unwrap();
        assert_eq!(done.state, JobState::Printed);
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!((stats.queued, stats.printing, stats.processed), (0, 0, 1));
        assert_eq!(stats.pages_printed, 2);
        assert_eq!(stats.failed, 0);
        // The text is released once the job finishes.
        assert!(s.source("main", 1).is_none());
    }

    #[test]
    fn jobs_are_handed_out_in_submission_order() {
        let s = store("");
        for _ in 0..3 {
            submit(&s, "main", "t1");
            // Each team may only have a limited number queued, so complete one.
            if let Some(c) = s.claim("main", "c", NOW) {
                s.complete("main", c.id, true, None, NOW).unwrap();
            }
        }
        let s = store("");
        submit(&s, "main", "t1");
        submit(&s, "main", "t2");
        assert_eq!(s.claim("main", "c", NOW).unwrap().id, 1);
        assert_eq!(s.claim("main", "c", NOW).unwrap().id, 2);
        assert!(s.claim("main", "c", NOW).is_none());
    }

    #[test]
    fn queues_are_isolated_per_site() {
        let s = store("");
        submit(&s, "main", "t1");
        assert!(s.claim("annex", "c", NOW).is_none());
        assert_eq!(s.stats("annex", NOW).unwrap().queued, 0);
        assert_eq!(s.claim("main", "c", NOW).unwrap().id, 1);
        assert!(s.stats("nosuch", NOW).is_none());
        assert!(s.claim("nosuch", "c", NOW).is_none());
    }

    #[test]
    fn failures_are_recorded_with_the_client_error() {
        let s = store("");
        submit(&s, "main", "t1");
        let c = s.claim("main", "c", NOW).unwrap();
        let view = s
            .complete("main", c.id, false, Some("no paper".into()), NOW + 2)
            .unwrap();
        assert_eq!(view.state, JobState::Failed);
        assert_eq!(view.error.as_deref(), Some("no paper"));
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!((stats.processed, stats.failed), (0, 1));
        assert_eq!(stats.pages_printed, 0);
    }

    #[test]
    fn completion_is_rejected_unless_the_job_is_claimed() {
        let s = store("");
        submit(&s, "main", "t1");
        assert_eq!(
            s.complete("main", 1, true, None, NOW),
            Err(CompleteError::NotClaimed(JobState::Queued))
        );
        assert_eq!(
            s.complete("main", 99, true, None, NOW),
            Err(CompleteError::UnknownJob)
        );
        assert_eq!(
            s.complete("nosuch", 1, true, None, NOW),
            Err(CompleteError::UnknownSite)
        );

        let c = s.claim("main", "c", NOW).unwrap();
        s.complete("main", c.id, true, None, NOW).unwrap();
        // A duplicate report must not double-count.
        assert_eq!(
            s.complete("main", c.id, true, None, NOW),
            Err(CompleteError::NotClaimed(JobState::Printed))
        );
        assert_eq!(s.stats("main", NOW).unwrap().processed, 1);
    }

    #[test]
    fn per_team_queue_limit_is_enforced() {
        let s = store("limits:\n  max_queued_per_team: 2\n");
        submit(&s, "main", "t1");
        submit(&s, "main", "t1");
        assert_eq!(
            s.check_capacity("main", "t1"),
            Err(SubmitError::TooManyQueued { limit: 2 })
        );
        assert_eq!(
            s.submit(
                "main",
                s.reserve_id(),
                "t1",
                "Team One",
                "x".into(),
                meta(),
                NOW
            ),
            Err(SubmitError::TooManyQueued { limit: 2 })
        );
        // A different team is unaffected.
        assert!(s.check_capacity("main", "t2").is_ok());
        submit(&s, "main", "t2");

        // Finishing a job frees a slot; a claimed-but-unfinished job does not.
        let c = s.claim("main", "c", NOW).unwrap();
        assert_eq!(
            s.check_capacity("main", "t1"),
            Err(SubmitError::TooManyQueued { limit: 2 })
        );
        s.complete("main", c.id, true, None, NOW).unwrap();
        assert!(s.check_capacity("main", "t1").is_ok());
    }

    #[test]
    fn an_expired_lease_requeues_the_job() {
        let s = store("limits:\n  lease_seconds: 30\n  max_attempts: 3\n");
        submit(&s, "main", "t1");
        let c = s.claim("main", "dead-client", NOW).unwrap();
        assert_eq!(c.attempt, 1);

        // Still within the lease: nothing happens.
        assert_eq!(s.sweep(NOW + 20), 0);
        assert_eq!(s.stats("main", NOW).unwrap().printing, 1);

        assert_eq!(s.sweep(NOW + 31), 1);
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!((stats.queued, stats.printing, stats.failed), (1, 0, 0));

        // A healthy client can pick it up again.
        let c = s.claim("main", "live-client", NOW + 32).unwrap();
        assert_eq!(c.attempt, 2);
        s.complete("main", c.id, true, None, NOW + 33).unwrap();
        assert_eq!(s.stats("main", NOW).unwrap().processed, 1);
    }

    #[test]
    fn a_job_that_keeps_losing_its_lease_eventually_fails() {
        let s = store("limits:\n  lease_seconds: 10\n  max_attempts: 2\n");
        submit(&s, "main", "t1");
        let mut t = NOW;
        for expected_attempt in 1..=2 {
            let c = s.claim("main", "flaky", t).unwrap();
            assert_eq!(c.attempt, expected_attempt);
            t += 11;
            s.sweep(t);
        }
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!((stats.queued, stats.printing, stats.failed), (0, 0, 1));
        let job = &s.team_jobs("main", "t1", 10)[0];
        assert_eq!(job.state, JobState::Failed);
        assert!(job.error.as_deref().unwrap().contains("did not confirm"));
        assert!(s.claim("main", "flaky", t).is_none());
    }

    #[test]
    fn history_is_trimmed_but_counters_persist() {
        let s = store("limits:\n  job_history: 3\n  max_queued_per_team: 1\n");
        for _ in 0..6 {
            submit(&s, "main", "t1");
            let c = s.claim("main", "c", NOW).unwrap();
            s.complete("main", c.id, true, None, NOW).unwrap();
        }
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!(stats.processed, 6);
        assert_eq!(stats.submitted, 6);
        // Only the last three jobs are retained for display.
        let jobs = s.team_jobs("main", "t1", 100);
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs.iter().map(|j| j.id).collect::<Vec<_>>(), vec![6, 5, 4]);
    }

    #[test]
    fn team_views_are_scoped_to_the_team_and_newest_first() {
        let s = store("");
        submit(&s, "main", "t1");
        submit(&s, "main", "t2");
        submit(&s, "main", "t1");
        let t1 = s.team_jobs("main", "t1", 10);
        assert_eq!(t1.iter().map(|j| j.id).collect::<Vec<_>>(), vec![3, 1]);
        assert_eq!(s.team_jobs("main", "t2", 10).len(), 1);
        assert_eq!(s.team_jobs("main", "nobody", 10).len(), 0);
        assert_eq!(s.recent_jobs("main", 2).len(), 2);
        assert_eq!(s.team_jobs("main", "t1", 1).len(), 1);
    }

    #[test]
    fn two_clients_on_one_token_share_the_queue() {
        let s = store("limits:\n  max_queued_per_team: 5\n");
        for _ in 0..4 {
            submit(&s, "main", "t1");
        }

        // Each job goes to exactly one client: they are workers, not mirrors.
        let a1 = s.claim("main", "printer-a", NOW).unwrap();
        let b1 = s.claim("main", "printer-b", NOW).unwrap();
        let a2 = s.claim("main", "printer-a", NOW + 1).unwrap();
        let b2 = s.claim("main", "printer-b", NOW + 2).unwrap();
        let ids = [a1.id, b1.id, a2.id, b2.id];
        assert_eq!(ids, [1, 2, 3, 4], "jobs must not be handed out twice");
        assert!(s.claim("main", "printer-a", NOW).is_none());

        // Both are visible, newest poll first, with what each has taken.
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!(stats.clients.len(), 2);
        assert_eq!(stats.clients[0].name, "printer-b");
        assert_eq!(stats.clients[0].last_seen, NOW + 2);
        assert_eq!(stats.clients[0].claimed, 2);
        assert_eq!(stats.clients[1].name, "printer-a");
        assert_eq!(stats.clients[1].claimed, 2);

        // Each confirms its own jobs.
        s.complete("main", a1.id, true, None, NOW + 3).unwrap();
        s.complete("main", b1.id, true, None, NOW + 3).unwrap();
        assert_eq!(s.stats("main", NOW).unwrap().processed, 2);
    }

    #[test]
    fn the_client_list_does_not_grow_without_bound() {
        let s = store("");
        for i in 0..40 {
            s.touch_client("main", &format!("client-{i}"), NOW + i);
        }
        let stats = s.stats("main", NOW).unwrap();
        assert!(stats.clients.len() <= 16, "{} tracked", stats.clients.len());
        // The most recent one is always kept.
        assert_eq!(stats.clients[0].name, "client-39");
    }

    /// Rebuilds a store from what a journal recorded, as a restart would.
    fn restart(path: &std::path::Path, extra: &str) -> Store {
        let (journal, events) = Journal::open(path).unwrap();
        Store::new(&config(extra)).with_journal(journal, events)
    }

    fn state_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("webprint-queue-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.jsonl")
    }

    #[test]
    fn a_restart_keeps_the_queue() {
        let path = state_path("restart");
        let extra = "limits:\n  max_queued_per_team: 5\n";

        {
            let s = restart(&path, extra);
            for n in 1..=3 {
                let id = s.reserve_id();
                s.submit(
                    "main",
                    id,
                    "t1",
                    "Team One",
                    format!("job {n}").into(),
                    meta(),
                    NOW,
                )
                .unwrap();
            }
            // One is printed, one is in flight when the server stops.
            let first = s.claim("main", "printer", NOW).unwrap();
            s.complete("main", first.id, true, None, NOW + 1).unwrap();
            let _in_flight = s.claim("main", "printer", NOW + 2).unwrap();
        }

        let s = restart(&path, extra);
        let stats = s.stats("main", NOW + 10).unwrap();
        // The printed job is gone; the other two are waiting, including the
        // one whose client disappeared mid-print.
        assert_eq!(stats.queued, 2, "{stats:?}");
        assert_eq!(stats.printing, 0);
        assert_eq!(stats.processed, 1, "counters must survive too");
        assert_eq!(stats.submitted, 3);
        assert_eq!(stats.pages_printed, 2);

        // The text came back, so the documents can still be built.
        let next = s.claim("main", "printer", NOW + 11).unwrap();
        assert_eq!(next.id, 2);
        assert_eq!(&*s.source("main", 2).unwrap().content, "job 2");
        // Its earlier attempt is remembered, so a job cannot retry for ever.
        assert_eq!(next.attempt, 2);

        // New jobs carry on after the restored ones.
        let id = s.reserve_id();
        assert_eq!(id, 4);
    }

    #[test]
    fn a_restart_without_a_journal_starts_empty() {
        let s = store("");
        submit(&s, "main", "t1");
        drop(s);
        let s = store("");
        assert_eq!(s.stats("main", NOW).unwrap().queued, 0);
    }

    #[test]
    fn the_state_file_does_not_grow_across_restarts() {
        let path = state_path("compaction");
        let extra = "limits:\n  max_queued_per_team: 50\n";
        for round in 0..5 {
            let s = restart(&path, extra);
            for _ in 0..10 {
                let id = s.reserve_id();
                s.submit("main", id, "t1", "Team One", "x".into(), meta(), NOW)
                    .unwrap();
                let claimed = s.claim("main", "printer", NOW).unwrap();
                s.complete("main", claimed.id, true, None, NOW).unwrap();
            }
            let lines = std::fs::read_to_string(&path).unwrap().lines().count();
            assert!(
                lines < 40,
                "round {round}: the file grew to {lines} lines despite compaction"
            );
        }
        // Everything that happened is still counted.
        let s = restart(&path, extra);
        assert_eq!(s.stats("main", NOW).unwrap().processed, 50);
    }

    #[test]
    fn jobs_for_a_site_that_is_gone_are_dropped() {
        let path = state_path("missing-site");
        {
            let s = restart(&path, "");
            let id = s.reserve_id();
            s.submit("annex", id, "a1", "Annex One", "x".into(), meta(), NOW)
                .unwrap();
        }
        // The annex is not in this configuration any more.
        let yaml = "sites:\n  main:\n    token: tok\n    teams:\n      \
                    - {username: t1, password: p, name: Team One}\n";
        let (journal, events) = Journal::open(&path).unwrap();
        let s = Store::new(&Config::parse_str(yaml).unwrap()).with_journal(journal, events);
        assert!(s.stats("annex", NOW).is_none());
        assert_eq!(s.stats("main", NOW).unwrap().queued, 0);
    }

    #[test]
    fn stats_report_oldest_queue_age_and_per_team_breakdown() {
        let s = store("");
        s.submit(
            "main",
            s.reserve_id(),
            "t1",
            "Team One",
            "x".into(),
            meta(),
            NOW - 90,
        )
        .unwrap();
        s.submit(
            "main",
            s.reserve_id(),
            "t2",
            "Team Two",
            "x".into(),
            meta(),
            NOW - 10,
        )
        .unwrap();
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!(stats.oldest_queued_age, Some(90));
        assert_eq!(stats.per_team.len(), 2);
        assert!(stats.per_team.iter().all(|t| t.queued == 1));

        let all = s.all_stats(NOW);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].site, "annex");
        assert_eq!(all[1].site, "main");
    }

    #[tokio::test]
    async fn claim_wait_returns_as_soon_as_a_job_arrives() {
        let s = Arc::new(store(""));
        let bg = {
            let s = s.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                submit(&s, "main", "t1");
            })
        };
        let started = std::time::Instant::now();
        let job = s
            .claim_wait("main", "c", std::time::Duration::from_secs(5))
            .await
            .expect("job delivered");
        assert_eq!(job.id, 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        bg.await.unwrap();
    }

    #[tokio::test]
    async fn claim_wait_times_out_when_idle_and_records_the_poll() {
        let s = store("");
        let started = std::time::Instant::now();
        assert!(s
            .claim_wait("main", "idle-client", std::time::Duration::from_millis(50))
            .await
            .is_none());
        assert!(started.elapsed() >= std::time::Duration::from_millis(50));
        let stats = s.stats("main", NOW).unwrap();
        assert_eq!(stats.clients.len(), 1);
        assert_eq!(stats.clients[0].name, "idle-client");
        assert_eq!(stats.clients[0].claimed, 0);
    }

    #[tokio::test]
    async fn claim_wait_picks_up_a_job_queued_before_the_wait() {
        let s = store("");
        submit(&s, "main", "t1");
        let job = s
            .claim_wait("main", "c", std::time::Duration::from_millis(50))
            .await;
        assert_eq!(job.map(|j| j.id), Some(1));
    }
}
