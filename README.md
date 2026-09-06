# ft-man

A terminal UI for managing [FreeToken](https://github.com/FlashML-org/FreeToken) on the
machine it runs on. Browse and download checkpoints from Hugging Face, convert them to
FreeToken's FTW format, configure every `ft serve` knob, launch and supervise the engine,
retune its cache pools without a restart, and watch throughput, requests and logs — from
one screen.

```
 ft-man  1 Dashboard  2 Models  3 Hub  4 Serve  5 Cache  6 Jobs  7 Requests  8 Logs   ● serving · Qwen3.6-35B-A3B
╭ Engine ───────────────────────────────────╮╭ GPU (NVML) ───────────────────────────────╮
│Status            ● serving                ││0 NVIDIA GeForce RTX 5090   ← engine       │
│Model             Qwen3.6-35B-A3B          ││  VRAM    ▕██████████████████▉··▏  87%     │
│Endpoint          http://127.0.0.1:1919    ││  Util    ▕███████████████████▊·▏  96%     │
│Process           pid 48211                ││  71°C   498 / 575 W   gen5 x16            │
│Uptime            1h 10m                   ││                                           │
│Shape             256k ctx · hybrid_linear ││bench: nvfp4→hybrid                        │
╰───────────────────────────────────────────╯╰───────────────────────────────────────────╯
╭ Throughput ───────────────────────────────╮╭ Activity ─────────────────────────────────╮
│Decode            48.7 tok/s   peak 61.2   ││In flight         2    0.31 completed/s    │
│  ▂▃▅▆▇█▇▆▅▄▃▄▅▆▇█▇▆▅▃▂▃▄▅▆▇▆▅▄▃▂▃▄▅▆▇█▇▆  ││Completed         1,487                    │
│Prefill        3,120.5 tok/s               ││Latency           p95 8,340 ms  TTFT 412 ms│
╰───────────────────────────────────────────╯╰───────────────────────────────────────────╯
```

## Why

FreeToken's engine resolves almost everything automatically, which is the right default
and also means the knobs that matter are invisible until you need them. `ft-man` puts the
whole surface in one place: every flag with its type, range, default and what it actually
does; the elastic cache resize that otherwise takes a hand-written `curl`; the bandwidth
profile that decides `--moe-backend auto` between offload and hybrid; and the process
supervision that keeps a serve alive across sessions.

## Requirements

- Linux x86_64 (developed against Debian 13), NVIDIA GPU
- A working FreeToken install — see [its install guide](https://github.com/FlashML-org/FreeToken/blob/main/docs/install.md)
- Rust 1.85+ to build

`ft-man` drives FreeToken's own CLI and HTTP API; it does not link against or vendor any
of it, and it needs no Python of its own.

## Install

```bash
git clone https://github.com/jlbyh2o/ft-man && cd ft-man
cargo build --release
install -Dm755 target/release/ft-man ~/.local/bin/ft-man
```

## Getting started

```bash
ft-man --doctor      # what it found: the ft binary, your GPUs, your checkpoints
ft-man               # the UI
```

FreeToken normally lives in a virtualenv, so `ft` is usually not on your PATH.
`--doctor` says whether it was found and where. If it was not, point at it once:

```bash
ft-man --venv ~/FreeToken/.venv
```

...then write that into the config so you do not have to repeat it:

```bash
ft-man --init-config
$EDITOR ~/.config/ft-man/config.toml
```

## The screens

| Tab | What it is for |
|---|---|
| **Dashboard** | Engine state, throughput, cache pools, GPU and host telemetry. The screen you leave open. |
| **Models** | Your local checkpoint library. Recognizes HF, FTW and GGUF, pairs a checkpoint with its FTW build, and says what to do with each. |
| **Hub** | Search Hugging Face, pick files, download. Resumable, parallel, and it skips duplicate weight formats by default. |
| **Serve** | Every `ft serve` flag, grouped, with its domain and help text. Save configurations as named profiles. |
| **Cache** | Resize the MoE, KV, GDN and SWA pools on the running engine, with the VRAM cost of each change shown before you apply it. |
| **Jobs** | FTW conversions, bandwidth benchmarks and downloads, with real progress bars and live output. |
| **Requests** | The engine's request ring: status, latency, TTFT and token counts per call. |
| **Logs** | The engine's output, filterable, with an errors-only toggle and a detachable tail. |

Press `?` anywhere for the full key map.

## What it does that is worth knowing

**It owns the engine, and it lets go.** `ft serve` is started in its own session, its
output is teed to a file under `~/.local/state/ft-man/logs`, and `{pid, starttime, args}`
is recorded so a later `ft-man` re-attaches to a serve that is still running. Quitting
`ft-man` deliberately leaves the engine up — a loaded 200 GiB model should not die because
a TUI closed. Only a stop you asked for stops it.

**Progress is real, not a spinner.** `ft checkpoint` and `ft bench bw` both emit a
machine-readable line protocol when asked (`FTCONVERT`, `FTBENCH`); `ft-man` sets those
env vars and parses them, so a two-hour conversion shows actual bytes against actual
totals.

**The knob table is the documentation.** Every flag carries its type, range, default,
help text and mutual exclusions in one schema. Setting `--moe-cache-size` clears
`--moe-cache-rate` for you, because the engine would reject the pair.

**It polls where the engine will actually be.** If a profile pins port 1920, `ft-man`
polls 1920. A bind address of `0.0.0.0` is polled over loopback, because a wildcard is not
a destination.

**GPU exclusivity is enforced up front.** A conversion or a benchmark needs the card, so
both refuse to start while an engine is running — with a message saying so, rather than a
CUDA OOM ten seconds in.

## Configuration

`~/.config/ft-man/config.toml`, written on first run. `FT_MAN_CONFIG_DIR` and
`FT_MAN_STATE_DIR` relocate it, which is how you run more than one independent setup on a
machine.

```toml
[freetoken]
# Where FreeToken lives. Set one of these if `ft` is not on PATH.
# binary = "/opt/freetoken/.venv/bin/ft"
venv = "/home/you/FreeToken/.venv"
# env = [["CUDA_HOME", "/usr/local/cuda"]]

[server]
host = "127.0.0.1"     # also the default `ft serve --host`
port = 1919            # also the default `ft serve --port`
poll_ms = 1000
timeout_ms = 4000

[library]
roots = ["/home/you/models", "/srv/models"]
download_dir = "/home/you/models"

[hub]
endpoint = "https://huggingface.co"
# token = "hf_..."     # or set HF_TOKEN; the `hf` CLI's cached token is also read
concurrency = 4
ignore = ["*.bin", "*.pth", "*.msgpack", "*.h5", "*.onnx"]

[ui]
theme = "auto"                # auto | dark | light | mono
tick_ms = 200
log_capacity = 5000
confirm_destructive = true
```

Profiles live beside it in `profiles.toml` and are plain TOML — a profile records only the
knobs you set, so "leave the MoE backend on auto" survives a FreeToken upgrade that
changes what auto means.

## Command line

```
ft-man [--host HOST] [--port PORT] [--venv DIR] [--ft-binary PATH]
       [--models DIR]... [--theme NAME] [--tab TAB] [--doctor] [--init-config]
```

CLI flags override the config file for that run and are not written back.

## A typical first run

1. **Hub** → `/` → search `Qwen3.6-35B-A3B` → Enter → `d`. It downloads to
   `library.download_dir`.
2. **Models** → select it → `c`. Converts to FTW in a sibling `-ftw` directory. Watch it
   on **Jobs**.
3. **Jobs** → `b`. Runs `ft bench bw` once for this machine, so `--moe-backend auto` can
   choose hybrid over offload when your RAM bandwidth justifies it.
4. **Models** → select the checkpoint → Enter. It loads the FTW build into **Serve**.
5. **Serve** → adjust anything; `p` shows the exact command line → `g` to start.
6. **Cache** → once it is serving, trade KV capacity against resident experts and apply
   without a restart.

## Development

```bash
cargo test        # 74 tests, including render and input sweeps across five terminal sizes
cargo clippy --all-targets
cargo fmt
```

The render tests draw every view at 40×12 through 200×60, in every theme, empty and
populated, with each overlay open — and the input tests press every printable and
navigation key on every tab. A TUI that panics mid-draw corrupts the terminal, so that is
the failure mode most worth spending tests on.

## License

Apache-2.0, matching FreeToken.
