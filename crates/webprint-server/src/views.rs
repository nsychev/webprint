//! HTML rendering. Pages are plain server-rendered HTML with inline CSS and a
//! little JavaScript, so a team's browser needs nothing but the page itself.

use crate::queue::{JobView, SiteStats};
use crate::timezone::TimeZone;
use crate::util::{format_age, format_time, format_ts, html_escape};

const STYLE: &str = r#"
:root {
  --bg: #f6f7f9; --panel: #ffffff; --ink: #14161a; --muted: #5d6470;
  --line: #d8dce3; --accent: #1f6feb; --accent-ink: #ffffff;
  --ok: #117a42; --warn: #8a5a00; --err: #b3261e;
  --mono: "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #15171c; --panel: #1d2026; --ink: #e8eaee; --muted: #9aa3b2;
    --line: #313743; --accent: #4c8dff; --ok: #4ec07e; --warn: #e0a93a; --err: #ff6b5e;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg); color: var(--ink); font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif;
}
.wrap { max-width: 980px; margin: 0 auto; padding: 24px 16px 56px; }
.center { max-width: 420px; margin: 8vh auto; padding: 0 16px; }
h1 { font-size: 20px; margin: 0; letter-spacing: -0.01em; }
h2 { font-size: 14px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); margin: 28px 0 10px; }
h3 { font-size: inherit; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); margin: 18px 0 0; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; padding: 20px; }
.bar { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; flex-wrap: wrap; margin-bottom: 18px; }
.sub { color: var(--muted); font-size: 13px; }
label { display: block; font-size: 13px; color: var(--muted); margin: 14px 0 6px; }
input, textarea {
  width: 100%; padding: 9px 11px; border: 1px solid var(--line); border-radius: 8px;
  background: var(--bg); color: var(--ink); font-size: 14px; font-family: inherit;
}
textarea { font-family: var(--mono); font-size: 13px; line-height: 1.45; min-height: 46vh; resize: vertical; white-space: pre; overflow-wrap: normal; overflow-x: auto; }
input:focus, textarea:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
button {
  padding: 10px 18px; border: 0; border-radius: 8px; background: var(--accent); color: var(--accent-ink);
  font-size: 15px; font-weight: 600; cursor: pointer; font-family: inherit;
}
button:disabled { opacity: 0.55; cursor: progress; }
button.ghost { background: transparent; color: var(--muted); border: 1px solid var(--line); font-weight: 500; }
.actions { display: flex; gap: 12px; align-items: center; margin-top: 16px; flex-wrap: wrap; }
.msg { padding: 10px 12px; border-radius: 8px; font-size: 14px; margin-top: 14px; border: 1px solid transparent; }
.msg.ok { color: var(--ok); border-color: currentColor; }
.msg.err { color: var(--err); border-color: currentColor; }
.msg:empty { display: none; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th { text-align: left; font-weight: 600; color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: 0.04em; }
th, td { padding: 7px 10px 7px 0; border-bottom: 1px solid var(--line); }
td.num, th.num { font-variant-numeric: tabular-nums; }
.state.queued { color: var(--muted); }
.state.printing { color: var(--accent); }
.state.printed { color: var(--ok); }
.state.failed { color: var(--err); font-weight: 600; }
.hint { color: var(--muted); font-size: 12.5px; margin-top: 10px; }
code { font-family: var(--mono); font-size: 12.5px; }
a { color: var(--accent); }
"#;

/// Wraps a body in the shared document shell.
fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"robots\" content=\"noindex\">\n\
         <title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n{body}\n</body></html>\n",
        html_escape(title)
    )
}

/// The sign-in page.
pub fn login(base: &str, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!("<div class=\"msg err\">{}</div>", html_escape(e)))
        .unwrap_or_default();

    page(
        "Sign in · webprint",
        &format!(
            "<div class=\"center\">\n\
             <div class=\"bar\"><h1>webprint</h1></div>\n\
             <div class=\"card\">\n\
             <form method=\"post\" action=\"{base}/login\" autocomplete=\"on\">\n\
             <label for=\"u\">Username</label>\n\
             <input id=\"u\" name=\"username\" autocapitalize=\"none\" autocorrect=\"off\" \
             autocomplete=\"username\" required autofocus>\n\
             <label for=\"p\">Password</label>\n\
             <input id=\"p\" name=\"password\" type=\"password\" autocomplete=\"current-password\" required>\n\
             <div class=\"actions\"><button type=\"submit\">Sign in</button></div>\n\
             {error_html}\n\
             </form>\n</div></div>"
        ),
    )
}

/// The print page.
#[allow(clippy::too_many_arguments)]
pub fn print_page(
    base: &str,
    team: &str,
    site_name: &str,
    jobs: &[JobView],
    max_pages: usize,
    tz: TimeZone,
    notice: Option<(bool, &str)>,
) -> String {
    let notice_html = match notice {
        Some((true, msg)) => format!("<div class=\"msg ok\">{}</div>", html_escape(msg)),
        Some((false, msg)) => format!("<div class=\"msg err\">{}</div>", html_escape(msg)),
        None => String::new(),
    };

    let body = format!(
        "<div class=\"wrap\">\n\
         <div class=\"bar\">\n\
         <div><h1>{team}</h1><div class=\"sub\">{site_name}</div></div>\n\
         <form method=\"post\" action=\"{base}/logout\"><button class=\"ghost\" type=\"submit\">Sign out</button></form>\n\
         </div>\n\
         <div class=\"card\">\n\
         <form id=\"printForm\" method=\"post\" action=\"{base}/print\">\n\
         <label for=\"content\">Text to print</label>\n\
         <textarea id=\"content\" name=\"content\" spellcheck=\"false\" autofocus autocomplete=\"off\"></textarea>\n\
         <div class=\"actions\">\n\
         <button id=\"printBtn\" type=\"submit\">Print</button>\n\
         <span class=\"sub\" id=\"counter\">0 characters</span>\n\
         </div>\n\
         <div class=\"msg\" id=\"status\">{notice_html}</div>\n\
         </form>\n\
         <p class=\"hint\">Up to {max_pages} pages per job. \
         Press <code>Ctrl</code>+<code>Enter</code> to print.</p>\n\
         </div>\n\
         <h2>Your print jobs</h2>\n\
         <div class=\"card\" id=\"jobs\">{jobs_table}</div>\n\
         <script>{script}</script>\n\
         </div>",
        team = html_escape(team),
        site_name = html_escape(site_name),
        jobs_table = jobs_table(jobs, tz),
        // The script talks to the same prefix the page was served from.
        script = PRINT_SCRIPT.replace("__BASE__", base),
        base = html_escape(base),
    );
    page(&format!("Print · {team}"), &body)
}

/// Renders the team's job list.
pub fn jobs_table(jobs: &[JobView], tz: TimeZone) -> String {
    if jobs.is_empty() {
        return "<p class=\"sub\">No print jobs yet.</p>".to_string();
    }
    let rows = jobs
        .iter()
        .map(|j| {
            let detail = match &j.error {
                Some(err) => format!("<div class=\"sub\">{}</div>", html_escape(err)),
                None => String::new(),
            };
            format!(
                "<tr><td class=\"num\">{id}</td><td>{time}</td>\
                 <td class=\"num\">{pages}</td>\
                 <td><span class=\"state {state}\">{state}</span>{detail}</td></tr>",
                id = j.id,
                time = format_time(j.created_at, tz),
                pages = j.pages,
                state = j.state.label(),
            )
        })
        .collect::<String>();
    format!(
        "<table><thead><tr><th>ID</th><th>Submitted</th>\
         <th>Pages</th><th>Status</th></tr></thead>\
         <tbody>{rows}</tbody></table>"
    )
}

/// Guards against double submits, keeps the character counter current, and
/// refreshes the job list. Without JavaScript the plain form post still works.
const PRINT_SCRIPT: &str = r#"
(function () {
  var form = document.getElementById('printForm');
  var btn = document.getElementById('printBtn');
  var area = document.getElementById('content');
  var status = document.getElementById('status');
  var counter = document.getElementById('counter');
  var jobs = document.getElementById('jobs');
  var sending = false;

  function setStatus(kind, text) {
    status.className = 'msg ' + kind;
    status.textContent = text;
  }

  function count() {
    var n = area.value.length;
    counter.textContent = n.toLocaleString() + (n === 1 ? ' character' : ' characters');
  }
  area.addEventListener('input', count);
  count();

  async function refresh() {
    try {
      var res = await fetch('__BASE__/api/my/jobs', { headers: { 'Accept': 'application/json' } });
      if (res.ok) jobs.innerHTML = (await res.json()).html;
    } catch (e) { /* offline; try again on the next tick */ }
  }

  form.addEventListener('submit', async function (ev) {
    ev.preventDefault();
    if (sending) return;
    var content = area.value;
    if (!content.trim()) { setStatus('err', 'Nothing to print.'); return; }

    sending = true;
    btn.disabled = true;
    var label = btn.textContent;
    btn.textContent = 'Sending...';
    setStatus('', '');
    try {
      var res = await fetch('__BASE__/api/print', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
        body: JSON.stringify({ content: content })
      });
      var data = await res.json().catch(function () { return {}; });
      if (res.ok) {
        setStatus('ok', 'Job ' + data.id + ' queued - ' + data.pages +
          (data.pages === 1 ? ' page' : ' pages'));
        area.value = '';
        count();
      } else {
        setStatus('err', data.error || ('Could not print (HTTP ' + res.status + ').'));
      }
    } catch (e) {
      setStatus('err', 'Could not reach the server. Check your connection and try again.');
    } finally {
      sending = false;
      btn.disabled = false;
      btn.textContent = label;
      refresh();
    }
  });

  area.addEventListener('keydown', function (ev) {
    if ((ev.ctrlKey || ev.metaKey) && ev.key === 'Enter') {
      ev.preventDefault();
      form.requestSubmit ? form.requestSubmit() : form.dispatchEvent(new Event('submit', { cancelable: true }));
    }
  });

  setInterval(refresh, 4000);
})();
"#;

/// The cross-site stats page for operators.
pub fn stats_page(sites: &[(SiteStats, TimeZone)], now: i64, server_tz: TimeZone) -> String {
    let sections = sites
        .iter()
        .map(|(s, tz)| {
            let tz = *tz;
            let client = if s.clients.is_empty() {
                "<div class=\"msg err\">No clients connected.</div>"
                    .to_string()
            } else {
                let rows = s
                    .clients
                    .iter()
                    .map(|c| {
                        let age = now - c.last_seen;
                        let kind = if age <= 60 { "ok" } else { "err" };
                        format!(
                            "<tr><td><code>{}</code></td><td class=\"num\">{}</td>\
                             <td><span class=\"state {kind}\">{}</span> ago</td>\
                             <td class=\"sub\">{}</td></tr>",
                            html_escape(&c.name),
                            c.claimed,
                            format_age(age),
                            format_ts(c.last_seen, tz),
                        )
                    })
                    .collect::<String>();
                format!(
                    "<table><thead><tr><th>Print client</th><th>Jobs</th>\
                     <th>Last seen</th><th></th></tr></thead><tbody>{rows}</tbody></table>"
                )
            };
            let oldest = s
                .oldest_queued_age
                .map(|a| format_age(a))
                .unwrap_or("—".to_string());

            let team_rows = s
                .per_team
                .iter()
                .map(|t| {
                    format!(
                        "<tr><td>{}</td><td class=\"sub\">{}</td><td class=\"num\">{}</td>\
                         <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                        html_escape(&t.team),
                        html_escape(&t.username),
                        t.submitted,
                        t.printed,
                        t.failed,
                        t.queued
                    )
                })
                .collect::<String>();
            let teams = if team_rows.is_empty() {
                "<p class=\"sub\">No jobs submitted yet.</p>".to_string()
            } else {
                format!(
                    "<table><thead><tr><th>Team</th><th>Login</th><th>Jobs</th>\
                     <th>Printed</th><th>Failed</th>\
                     <th>Pending</th></tr></thead><tbody>{team_rows}</tbody></table>"
                )
            };

            format!(
                "<h2>{name}</h2>\n<div class=\"card\">\n\
                 <table><thead><tr><th>Submitted</th><th>In queue</th><th>Printing</th>\n\
                 <th>Completed</th><th>Failed</th><th>Pages</th><th>Teams</th><th>Wait time</th></tr></thead>\n\
                 <tbody><tr><td class=\"num\">{submitted}</td><td class=\"num\">{queued}</td>\n\
                 <td class=\"num\">{printing}</td><td class=\"num\">{processed}</td><td class=\"num\">{failed}</td>\n\
                 <td class=\"num\">{pages}</td><td class=\"num\">{teams_count}</td><td class=\"num\">{oldest}</td>\n\
                 </tr></tbody></table>\n<h3>Clients</h3>\n{client}\n<h3>Teams</h3>\n{teams}\n</div>",
                name = html_escape(&s.name),
                queued = s.queued,
                printing = s.printing,
                processed = s.processed,
                failed = s.failed,
                submitted = s.submitted,
                pages = s.pages_printed,
                teams_count = s.teams,
            )
        })
        .collect::<String>();

    page(
        "Stats · webprint",
        &format!(
            "<div class=\"wrap\"><div class=\"bar\"><h1>webprint stats</h1>\
             <div class=\"sub\">{}</div></div>{sections}\
             <p class=\"hint\">This page refreshes every 10 seconds.</p>\
             <script>setTimeout(function(){{location.reload();}}, 10000);</script></div>",
            format_ts(now, server_tz)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{ClientStatus, JobState, TeamStats};

    fn job(id: u64, state: JobState) -> JobView {
        JobView {
            id,
            state,
            team: "Team One".into(),
            username: "t1".into(),
            pages: 2,
            lines: 40,
            chars: 400,
            created_at: 1_762_171_200,
            attempts: 1,
            claimed_by: None,
            finished_at: None,
            error: None,
        }
    }

    #[test]
    fn the_sign_in_page_asks_only_for_credentials() {
        // Asserted on the fields rather than their labels, so rewording the
        // page does not break the test.
        let page = login("", None);
        assert!(page.contains("name=\"username\""));
        assert!(page.contains("name=\"password\""));
        // There is no site to pick: the credentials identify the team.
        assert!(!page.contains("name=\"site\""), "{page}");
        assert!(!page.contains("<select"), "{page}");
    }

    #[test]
    fn the_sign_in_page_shows_an_error() {
        let page = login("", Some("Wrong login or password."));
        assert!(page.contains("Wrong login or password."));
        assert!(page.contains("msg err"));
    }

    #[test]
    fn user_supplied_text_is_escaped_everywhere() {
        let evil = "<script>alert(1)</script>";
        let page = login("", Some(evil));
        assert!(
            !page.contains("<script>alert(1)"),
            "login page is injectable"
        );
        // Once, in the error message: the page has nothing else to echo.
        assert_eq!(page.matches("&lt;script&gt;").count(), 1);

        let mut j = job(1, JobState::Failed);
        j.error = Some(evil.to_string());
        let page = print_page("", evil, evil, &[j], 10, TimeZone::UTC, Some((false, evil)));
        assert!(
            !page.contains("<script>alert(1)"),
            "print page is injectable"
        );
    }

    #[test]
    fn print_page_lists_jobs_and_limits() {
        let jobs = vec![job(7, JobState::Printed), job(8, JobState::Queued)];
        let page = print_page(
            "",
            "Team One",
            "Main Hall",
            &jobs,
            10,
            "UTC+03:00".parse().unwrap(),
            None,
        );
        assert!(page.contains("Team One"));
        assert!(page.contains("Main Hall"));
        // Teams do not choose a language: it is detected from the content.
        assert!(
            !page.contains("<select"),
            "the language picker should be gone"
        );
        assert!(!page.contains("name=\"language\""), "{page}");
        assert!(page.contains("Up to 10 pages"));
        assert!(page.contains(">printed<"));
        assert!(page.contains(">queued<"));
        // Both the no-JS form target and the JSON endpoint are present.
        assert!(page.contains("action=\"/print\""));
        assert!(page.contains("/api/print"));
    }

    #[test]
    fn a_base_path_prefixes_every_generated_url() {
        // Proxied under /print, every link the browser follows must carry it.
        let page = login("/print", None);
        assert!(page.contains("action=\"/print/login\""), "{page}");

        let page = print_page("/print", "T", "S", &[], 5, TimeZone::UTC, None);
        assert!(
            page.contains("action=\"/print/print\""),
            "missing form action"
        );
        assert!(
            page.contains("action=\"/print/logout\""),
            "missing logout action"
        );
        assert!(page.contains("'/print/api/print'"), "missing fetch target");
        assert!(
            page.contains("'/print/api/my/jobs'"),
            "missing refresh target"
        );
        // No unprefixed leftovers.
        assert!(
            !page.contains("action=\"/print\""),
            "unprefixed form action"
        );
        assert!(!page.contains("'/api/print'"), "unprefixed fetch target");
        assert!(!page.contains("__BASE__"), "placeholder left in the page");
    }

    #[test]
    fn at_the_root_no_prefix_is_added() {
        let page = print_page("", "T", "S", &[], 5, TimeZone::UTC, None);
        assert!(page.contains("action=\"/print\""), "{page}");
        assert!(page.contains("'/api/print'"));
        assert!(!page.contains("__BASE__"));
    }

    #[test]
    fn empty_job_list_reads_clearly() {
        let page = print_page("", "T", "S", &[], 5, TimeZone::UTC, None);
        assert!(page.contains("No print jobs yet."));
        assert!(jobs_table(&[], TimeZone::UTC).contains("No print jobs yet."));
    }

    #[test]
    fn job_table_shows_state_and_failure_detail() {
        let mut failed = job(3, JobState::Failed);
        failed.error = Some("printer offline".into());
        let html = jobs_table(&[failed], TimeZone::UTC);
        assert!(html.contains("printer offline"));
        assert!(html.contains("state failed"));
    }

    #[test]
    fn stats_page_flags_a_missing_or_stale_client() {
        let base = SiteStats {
            site: "main".into(),
            name: "Main Hall".into(),
            queued: 2,
            printing: 1,
            processed: 10,
            failed: 1,
            submitted: 14,
            pages_printed: 21,
            teams: 12,
            oldest_queued_age: Some(95),
            clients: Vec::new(),
            per_team: vec![TeamStats {
                username: "t1".into(),
                team: "Team One".into(),
                submitted: 3,
                printed: 2,
                failed: 0,
                queued: 1,
            }],
        };
        let now = 1_762_171_200;

        let page = stats_page(&[(base.clone(), TimeZone::UTC)], now, TimeZone::UTC);
        assert!(page.contains("No print client has polled"));
        assert!(page.contains("Main Hall"));
        assert!(page.contains("1m 35s"));
        assert!(page.contains("Team One"));

        let fresh = SiteStats {
            clients: vec![ClientStatus {
                name: "printer-1".into(),
                last_seen: now - 5,
                claimed: 9,
            }],
            ..base.clone()
        };
        let page = stats_page(&[(fresh, "UTC+03:00".parse().unwrap())], now, TimeZone::UTC);
        assert!(page.contains("printer-1"));
        assert!(
            page.contains("state ok"),
            "a live client should read as healthy"
        );
        // Each site is labelled with the zone its times are shown in.
        assert!(page.contains("UTC+03:00"), "{page}");

        let stale = SiteStats {
            clients: vec![ClientStatus {
                name: "printer-1".into(),
                last_seen: now - 600,
                claimed: 9,
            }],
            ..base.clone()
        };
        let page = stats_page(&[(stale, TimeZone::UTC)], now, TimeZone::UTC);
        assert!(page.contains("state err"), "a stale client must stand out");
        assert!(page.contains("10m 0s"));
    }

    #[test]
    fn several_clients_on_one_site_are_all_listed_and_explained() {
        let now = 1_793_707_200;
        let stats = SiteStats {
            site: "main".into(),
            name: "Main Hall".into(),
            queued: 0,
            printing: 0,
            processed: 10,
            failed: 0,
            submitted: 10,
            pages_printed: 12,
            teams: 3,
            oldest_queued_age: None,
            clients: vec![
                ClientStatus {
                    name: "printer-a".into(),
                    last_seen: now - 2,
                    claimed: 6,
                },
                ClientStatus {
                    name: "printer-b".into(),
                    last_seen: now - 9,
                    claimed: 4,
                },
            ],
            per_team: Vec::new(),
        };
        let page = stats_page(&[(stats, TimeZone::UTC)], now, TimeZone::UTC);
        assert!(page.contains("printer-a"), "{page}");
        assert!(page.contains("printer-b"), "{page}");
        assert!(page.contains("Jobs taken"));
        // And the shared-queue behaviour is spelled out.
        assert!(page.contains("share the queue"), "{page}");
    }
}
