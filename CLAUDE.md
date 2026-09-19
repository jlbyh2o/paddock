# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

paddock is a control panel for [FreeToken](https://github.com/FlashML-org/FreeToken) — an LLM inference engine. One binary serves both a terminal UI (ratatui) and a web UI (axum + React), backed by the same Rust application state. It manages checkpoints (HuggingFace cache + FTW builds), converts models, configures and supervises `ft serve`, resizes cache pools on a live engine, and monitors throughput, requests, and logs.

## Toolchain

- Rust 1.98 for day-to-day work (pinned in `mise.toml`); MSRV is 1.88 (declared in `Cargo.toml` and proven in CI).
- Node 24 for the web frontend.
- [mise](https://mise.jdx.dev) manages the toolchain: `mise install`.

## Building

```bash
# Rust only (embeds a placeholder HTML when no frontend is present)
cargo build                  # debug
cargo build --release        # release

# Full build (frontend + Rust, for releases)
cd web && npm ci && npm run build && cd ..
cargo build --release
```

`scripts/build-release.sh` does both steps and compiles Rust inside `rust:1.98.0-slim-bookworm` (glibc 2.36) so the binary runs on Debian 12+, Ubuntu 22.04+, RHEL 9+. Use `--skip-web` to reuse an existing `web/dist`.

## Testing

```bash
cargo test                    # full Rust suite (TUI render/input sweeps, web routes, everything)
cargo test --locked           # also pins Cargo.lock (CI does this)

cd web && npm run check       # TypeScript + Vitest
cd web && npm test            # Vitest only (no typecheck)
```

The Rust test suite is extensive: it sweeps every view at multiple terminal sizes, every theme, with every overlay open, and presses every printable and navigation key on every tab. The web suite runs TypeScript type-checking plus Vitest unit tests for format helpers.

## Linting & formatting

```bash
cargo fmt
cargo clippy --all-targets --locked -- -D warnings
```

CI requires `--locked` everywhere. The CI workflow pins to the exact compiler version (1.98.0) rather than stable, so a clean local run guarantees a green CI.

## Running

```bash
paddock --doctor              # diagnose: ft binary, GPUs, checkpoints, bench profile
paddock                       # terminal UI
paddock web                   # web UI (default 0.0.0.0:7979)
paddock web --listen 127.0.0.1:8000 --token secret
```

FreeToken typically lives in a virtualenv not on PATH. Point paddock at it once:
```bash
paddock --venv ~/FreeToken/.venv
paddock --init-config         # writes ~/.config/paddock/config.toml
```

## Configuration

`~/.config/paddock/config.toml` — `[freetoken]`, `[server]`, `[library]`, `[hub]`, `[ui]`, `[web]` sections. `paddock --init-config` writes every key at its default.

`~/.config/paddock/profiles.toml` — named serve configurations (save with `s` on the Serve tab).

`~/.local/state/paddock/` — mutable state: `serve.json` (engine PID, model, log path), captured logs, `costs.json` (measured cache costs).

Environment overrides: `PADDOCK_CONFIG_DIR`, `PADDOCK_STATE_DIR` (also `FT_MAN_*` for backward compat).

## Architecture

```
src/
  main.rs           — CLI parsing, doctor mode, event loop bootstrap
  config.rs         — XDG config/state paths, Config/Profiles structs, migration
  runtime.rs        — background task spawning (telemetry poller, engine watcher, job runner)
  actions.rs        — action dispatcher invoked by both TUI input and web routes

  ft/               — FreeToken integration layer
    mod.rs          — public types: Client, Engine, Job, EngineEvent, JobEvent
    proc.rs         — process management: start/stop/attach engine, spawn conversions/benchmarks
    api.rs          — HTTP client for ft serve's control plane (cache, health)
    locate.rs       — find `ft` binary via PATH, --ft-binary, or venv
    preflight.rs    — model fit check before download/conversion
    checkout.rs     — resolve a model name to a checkpoint path
    types.rs        — FreeToken HTTP API types

  ui/               — terminal UI (TUI)
    mod.rs
    app.rs          — App struct: owns all state, message types, Tab enum
    draw.rs         — ratatui render: frame → views
    input.rs        — key handler: dispatches to actions
    theme.rs        — color palette (auto/dark/light/mono)
    widgets.rs      — reusable TUI widgets: Confirm dialog, Selection, TextInput, Toast
    smoke.rs        — smoke test harness
    views/          — one module per tab (dashboard, models, hub, templates, serve, cache, jobs, requests, logs, help, plan, sampling)

  web/              — web daemon (paddock web)
    mod.rs          — run(), router(), background tasks (drain, ticker, SSE broadcaster)
    state.rs        — Shared<App> via tokio Mutex
    snapshot.rs     — deterministic snapshot for tests
    assets.rs       — serves embedded web/dist via rust-embed
    auth.rs         — Bearer token gate (subtle timing-safe comparison)
    guard.rs        — Axum layer for auth checking
    events.rs       — SSE broadcast to connected browsers
    routes/         — one module per domain (cache, engine, hub, jobs, meta, models, serve, streams, templates)
    tests.rs        — route snapshot tests

  hub.rs            — HuggingFace integration: search, repo info, download management
  knobs.rs          — `ft serve` flag definitions, grouping, help text
  models.rs         — checkpoint scanning, FTW format detection
  plan.rs           — cache cost planner: prices cache splits for context headroom
  probe.rs          — GPU (NVML) and host telemetry
  cache_pools.rs    — MoE/KV/GDN/SWA pool geometry and cost calculations
  variants.rs       — model variant resolution (e.g. Qwen3 variants)
  sampling.rs       — sampling defaults per checkpoint
  templates.rs      — chat template fetch/apply/revert
  compat.rs         — FreeToken version compatibility shims
  util.rs           — shared helpers (bytes formatting, EMA, history)

build.rs            — stages web/dist into OUT_DIR for rust-embed; writes placeholder if absent
```

Key design patterns:
- **Single shared `App`**: Both the TUI event loop and the web daemon run the same `App` type with the same message channel, ticker, and background tasks. The web routes call the same `actions.rs` functions that TUI keys do.
- **Background tasks**: Telemetry polling, engine watching, job monitoring, and download tracking all run as separate Tokio tasks, pushing `Message` events into an unbounded channel.
- **Engine lifecycle**: The engine survives paddock quitting. A later run re-attaches via `serve.json`. Only a user-initiated stop kills it.
- **`rust-embed`**: The web frontend (`web/dist`) is compiled into the binary at build time. `build.rs` never runs npm — the frontend is a separate step.

## Frontend

`web/` — React 19, TypeScript, Vite, Vitest. Minimal SPA (~2 files: `App.tsx`, `format.ts`). Builds to `web/dist/` which `build.rs` embeds.

```bash
cd web
npm run dev      # Vite dev server
npm run build    # production build
npm test         # Vitest
npm run check    # tsc --noEmit + vitest
```

## CI / Release

- `.github/workflows/ci.yml`: fmt check, clippy, test (all with `--locked`). MSRV job checks `cargo check --all-targets --locked` on Rust 1.88. Frontend is built explicitly before `cargo build`.
- `.github/workflows/release.yml`: builds release binary, attaches to GitHub release.

## Documentation

- `docs/design-notes.md` — design rationale (cache planning, prefix-cache estimation, GDN state pool)
- `docs/guides.md` — operational guides (template override, sampling defaults)
- `docs/web-ui.md` — browser UI architecture
- `docs/web-api.md` — HTTP API reference
- `docs/kv-geometry.md` — KV cache geometry details
- `docs/freetoken-compressed-tensors-moe.md` — NVFP4 MoE limitations
- `vendor-freetoken/` — vendored FreeToken source (agent docs, contributing guide)
