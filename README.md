# webprint

A printing service for ICPC-style contests.

```
   contestant                    server                     printing rooms
┌──────────────┐  browser  ┌─────────────────┐  polling  ┌─────────────────┐
│ sign in      │ ────────▶ │ webprint-server │ ◀──────── │ webprint-client │
│ paste, print │           │   renders PDF   │ ────────▶ │     prints      │
└──────────────┘           └─────────────────┘    job    └─────────────────┘
```

## Quick start

```bash
cargo build --release                      # builds server and client
```

You can also bake the server URL into a client binary:

```bash
WEBPRINT_DEFAULT_SERVER=https://example.org/print \
    cargo build --release -p webprint-client
```

### Server

```
cp config.example.yaml config.yaml         # then edit it
./target/release/webprint-server --config config.yaml --check
./target/release/webprint-server --config config.yaml
```

Teams can go to `http://<server>:8080/`, sign in, paste and print.

### Printing room

```bash
./webprint-client --server http://<server>:8080 [--token <TOKEN>]
# Site token: ********        (asked for when --token is not given)
```

## Server Configuration

See [`config.example.yaml`](config.example.yaml) for every option with comments.
You can use `--check` to validate the file.

A login and password together identify a team, so the pair must be unique
across the whole configuration — the server refuses to start otherwise. The
same login may appear more than once, as long as the passwords differ: at
different sites, or within one site to give a team both a practice and a
contest password.

A page holds 58 lines of up to about 81 characters, so the default 10-page
limit is 580 lines. Length is the only thing limited: a job over the limit is
refused when submitted, naming the page count it really needs, so nobody
collects a listing that stops in the middle. Lines are counted as printed, so
a line too long for the page counts once per printed line, and one very long
line can exceed the limit on its own.

The queue is kept on disk, so restarting the server does not lose jobs that
have not printed yet. The file lives beside the configuration unless
`state_file` says otherwise; a job that was printing when the server stopped
goes back to the queue.

If you're running behind a reverse proxy, you may need those options:
* `base_path: /print` — sets path that proxy uses
* `secure_cookies: true` — serves cookies over HTTPS

Use `/stats?token=<admin_token>` for statistics.

## Client

```
webprint-client [--server URL] [--token TOKEN] [--printer NAME] [OPTIONS]

  run           poll for jobs and print them (default)
  printers      list the printers this machine can see
```

You can run multiple clients with the same token.

The software tries to find a suitable printing method automatically. On Windows, download [SumatraPDF]
and place it in the same directory with `webprint-client`.

You can also choose a method using `--backend`:

| `--backend` | Description |
|---|---|
| `auto` (default) | Unix: `lp` or `lpr`; Windows: SumatraPDF or native spooler |
| `lp`, `lpr` | CUPS, or BSD-compatible `lpr` |
| `winspooler` | Windows Print Spooler, PDF supports depends on the printer |
| `sumatra`, `sumatra:<path>` | SumatraPDF |
| `ipp://…`, `ipps://…`, `https://…` | Any network printer |
| `command:<template>` | A command template with `{file}` and `{printer}` substitutions |
| `dry-run` | Nothing; the job is dropped |

For native backends, `--printer` may specify exact printer model to use.

## License

[The MIT License](LICENSE) covers the code.

JetBrains Mono is licensed under [the SIL Open Font License 1.1](crates/webprint-pdf/assets/JetBrainsMono-OFL.txt).

[SumatraPDF]: https://www.sumatrapdfreader.org/
