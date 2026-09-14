# The web interface

paddock 0.3 adds a browser interface with the same reach as the terminal one: every
screen, every action, the same confirmations, on the same state. This document records
the design decisions so that the two front ends stay one program with two faces rather
than drifting into two programs.

## What it is

```
paddock web                       # headless: serve the web UI on 0.0.0.0:7979
paddock web --listen 127.0.0.1:8000
paddock                           # the TUI, unchanged
```

`paddock web` runs the same `App` the TUI runs: the same telemetry poller, hardware
sampler, engine supervisor, job runner and download tracker, but instead of drawing to a
terminal it serves a single-page application over HTTP and streams state changes to it.
It is a long-lived daemon. Like the TUI it never stops an engine on exit unless a stop was
asked for, and it re-adopts a running engine from `serve.json` on startup.

Default port **7979**, chosen to sit well away from the engine's 1919 and the 192x range
FreeToken's own subprocesses use. `[web] listen` in the config and `--listen` on the
command line override it.

## Architecture

### One `App`, two front ends

All state lives in `crate::ui::app::App`, exactly as before. The web server holds one
`App` behind a mutex; HTTP handlers lock it, act, and unlock. A single task drains the
`Message` channel into `App::handle` and runs `App::tick` on the same cadence the TUI
does. Nothing is held across an `.await` while the lock is taken.

### Actions are shared, not duplicated

Every operation a key triggers in the TUI is extracted from `src/ui/input.rs` into
`src/actions.rs` as a function of `&mut App` plus an explicit target: a model *path*, a
job *id*, a profile *name*, a knob *key and value*, a template *name*. `input.rs` becomes
a key map that resolves "the selected row" to a target and calls the shared function; the
web layer resolves a request body to the same target and calls the same function. There
is one implementation of "convert this checkpoint", one of "start the engine", one of
"apply this template".

The TUI's confirmation flow is reused as-is: an action that needs a confirmation pushes
`app.confirm`, the snapshot carries it, the browser renders a modal, and
`POST /api/confirm` accepts or dismisses it. `ui.confirm_destructive` therefore applies
to both front ends.

Selection is a front-end concern. The TUI keeps its cursors in the `*View` structs as
before; the browser keeps its own selection and sends identities. The server never trusts
an index from the browser, because the list may have changed between render and request.

### Transport

- `GET /api/events` — Server-Sent Events. Each `snapshot` event carries the full
  `Snapshot` JSON; events are coalesced to at most ten a second and sent whenever the
  state changed, plus a heartbeat once a second. A browser that connects gets a snapshot
  immediately.
- `GET /api/snapshot` — the same document on demand.
- Append-only, unbounded-ish collections stay out of the snapshot and are fetched
  incrementally by sequence number: engine log lines, a job's output, the request ring.
- Everything else is `POST /api/...` with a JSON body and a JSON reply. Errors are
  `{ "error": "..." }` with a 4xx/5xx status. Actions that produce a toast in the TUI
  produce the same toast here; it arrives in the next snapshot.
- Optional bearer token (`[web] token`, `--token`): when set, every `/api` request must
  carry it as `Authorization: Bearer` or as the `paddock_token` cookie the login page
  sets. When unset there is no authentication, which is the `ft serve` default too.

The exact routes, bodies and the `Snapshot` shape are specified in
[web-api.md](web-api.md). `web/src/api/types.ts` is the TypeScript rendering of that
document and the frontend's source of truth.

### Frontend

`web/` is a React 19 + TypeScript application built with Vite. No component library: the
UI is dense tabular data with a handful of forms, and the TUI's layout translates almost
one to one. Hand-written CSS with light and dark themes following the system preference,
inline SVG sparklines, keyboard shortcuts mirroring the TUI's where they do not fight the
browser (`1`–`9` for tabs, `?` for help, `/` for search and filter).

The build output is embedded in the binary with `rust-embed`, so deployment stays a
single file. `build.rs` never runs `npm`: it embeds `web/dist` when `web/dist/index.html`
exists and otherwise embeds a placeholder page that says the frontend was not built and
how to build it. The frontend is built explicitly, before `cargo build`, by whoever is
producing a binary: `npm ci && npm run build` in `web/`, which CI, the release workflow
and `docker/build.sh` all do. A `cargo build` with no `web/dist` still succeeds, so the
Rust side can be developed and tested without Node.

### Tests

- The existing render and input sweeps keep passing; extracting the actions must not
  change what any key does.
- `src/web/` gets unit tests for snapshot construction from a populated `App` and for
  every route through `tower::ServiceExt::oneshot` against an `App` with no FreeToken,
  which is the state CI runs in.
- `web/` gets `tsc --noEmit` and a small `vitest` suite over the reducers and formatters.
  CI runs `npm ci && npm run check && npm run build` before `cargo test`.

## Sharing an engine between a TUI and the daemon

Both processes can run at once on one machine. Both adopt the engine from `serve.json`,
both poll it, and either can stop it. What one process starts as a *job* (a conversion,
a benchmark, a download) is visible only to that process — a job is a child process with
a pipe, and there is no state file for it. The web daemon is the natural home for
long-running jobs on a headless box; the TUI on the same box sees their effect in the
library once they finish.

When the daemon has no live engine and `serve.json` names a process that is alive, the
daemon adopts it on the next tick. The TUI does the same. Neither will start a second
engine while one is live, and the GPU-exclusivity check on conversions and benchmarks
looks at the adopted engine too.

## Deployment on a headless box

A systemd system unit running as the user who owns the FreeToken install. The full
template, with comments, is `contrib/paddock-web.service`:

```ini
[Unit]
Description=paddock web interface
After=network-online.target

[Service]
User=YOUR_USER
ExecStart=%h/.local/bin/paddock web
Environment=HF_HOME=/workspace/huggingface
Restart=on-failure
KillMode=process

[Install]
WantedBy=multi-user.target
```

`KillMode=process` matters: the engine runs in its own session under the daemon, and a
restart of the web service must not take a loaded model down with it.
