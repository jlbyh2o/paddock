# ft-man

A terminal UI for managing [FreeToken](https://github.com/FlashML-org/FreeToken) on the
machine it runs on. Browse and download checkpoints from Hugging Face, convert them to
FreeToken's FTW format, configure every `ft serve` knob, launch and supervise the engine,
retune its cache pools without a restart, and watch throughput, requests and logs — from
one screen.

```
 ft-man  1 Dashboard  2 Models  3 Hub  4 Templates  5 Serve  6 Cache  7 Jobs  8 Requests  9 Logs   ● serving · Qwen3.6-35B
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
| **Hub** | Search Hugging Face, check a repo against FreeToken *before* downloading it, pick files, download. Resumable and parallel. |
| **Templates** | Override a checkpoint's chat template with one fetched from a Hugging Face repo, and put the original back. |
| **Serve** | Every `ft serve` flag, grouped, with its domain and help text. `a` plans the launch against your hardware. Save configurations as named profiles. |
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

**It notices when you are serving a fraction of your context.** FreeToken sizes the KV
cache from whatever VRAM the expert cache leaves it — `--moe-cache-auto` reserves
`--kv-reserve-tokens` (8192 by default), lets the experts take the rest, and hands KV the
remainder. The engine then serves `min(model_ceiling, num_pages * page_size)` without
logging that it clamped, while `/v1/models` keeps advertising the checkpoint's full
ceiling on purpose. So a 256k model can be answering with 8k and nothing says so. The
Dashboard puts both numbers on one line and says which is real.

**And it can plan the fix.** Press `a` on the Serve tab. ft-man prices the cache split
from what the engine measured — `cache_budget_bytes` and the per-unit KV and expert costs
from `/v1/cache/status` — and works out the `--kv-reserve-tokens` that buys back the
context, what it costs in expert slots, and whether the card can reach the ceiling at all.
It also makes the two calls `auto` will not: `--moe-backend fused` when every expert
demonstrably fits in VRAM alongside full-context KV (the engine refuses to guess, because
a wrong guess is an OOM at weight load; ft-man has NVML and the geometry, so it is
arithmetic), and hybrid-vs-offload from the `ft bench bw` profile. Every line says why,
with the numbers it used. `A` applies it to the configuration you were already editing;
nothing is changed until you press it.

Costs are only knowable from a running engine, so ft-man writes down what each serve
measured (`~/.local/state/ft-man/costs.json`) and plans the next launch of that model
exactly. Before a model has ever been served, the plan says the split is unpriced rather
than inventing one.

**It estimates prefix-cache reuse, and says that it is an estimate.** FreeToken reports
the exact figure only in a completion's `usage.prompt_tokens_details` block, which ft-man
never sees — it polls the control plane rather than proxying model traffic, and neither
`/v1/stats` nor the request ring carries a cached-token count. So the Dashboard infers it:
time to first token is dominated by prefill, prefill only covers the uncached part of a
prompt, and the slowest request in the ring anchors the hardware's cold rate. A 70k prompt
reaching first token in 0.8 s on a card that prefills ~3k tokens/s did not prefill 70k
tokens. When the ring holds no spread between slow and fast requests the figure is withheld
rather than guessed, because a uniformly quick session and a quick GPU look identical from
outside.

**It prices the GDN state pool, which is easy to forget and expensive.** On a
hybrid-linear model (Qwen3.5-MoE and friends) the linear-attention state pool is a sibling
of the KV and expert pools, drawn from the same `cache_budget_bytes`, at tens of MiB per
slot — on a 16 GiB card that can be a seventh of the whole budget. Its size is a function
of `--max-running-requests`: four slots per running request plus a snapshot cache. So
concurrency is a *memory* knob on these models, not just a scheduling one, and the KV pool
may only have room for one full-length request anyway. When the expert cache is
budget-capped, the plan offers the trade with the arithmetic attached — on a 5070 Ti
serving a 35B-A3B at full context, dropping to one running request frees 982 MiB and grows
the expert cache from 1,822 to 2,403 slots. It stays quiet when every expert already fits,
because then the trade buys nothing.

**It knows host RAM is spent too.** The offload backends pin *every* expert in host RAM,
resident or not, so the host-side footprint is the whole model's experts rather than the
cache. ft-man reports when those banks dominate RAM, and a benched `hybrid` verdict yields
to a host with no headroom — hybrid's CPU executor needs working room on top of the banks,
and a faster backend that cannot allocate is not faster.

**A missing bandwidth profile is a speed ceiling with no symptom.** Without
`~/.cache/freetoken/benchbw/<gpu-uuid>.json`, `--moe-backend auto` can only ever resolve
to `offload`, and `--moe-hybrid-max-fetch auto` falls back to a fixed cap of 1 instead of
the bandwidth-matched split. `--doctor` and the plan both say so, and the benchmark is one
key on the Jobs tab.

**It says whether a repo can run here before you download it.** Pressing Enter on a Hub
result fetches only `config.json` — one small request — and reports a verdict. The
architecture check is definitive: FreeToken's own registry is queried at startup, so it is
never a stale list baked into ft-man. On top of that it catches the multimodal
quantization split described below, and weighs the download against this machine's VRAM,
host RAM and free disk. A clean result is not a promise the model will serve; it means
none of the known walls are in the way.

**A doomed conversion fails in seconds, not minutes.** Before running `ft checkpoint`,
ft-man resolves the checkpoint through FreeToken's own `EngineConfig` and compares what it
concluded against what the checkpoint declares. The case that motivated it: FreeToken's
expert-quantization detector for the Qwen3.5-MoE family reads `quant_algo`/`quant_method`
and never `format`, so an llm-compressor (`compressed-tensors`) NVFP4 export resolves to
`none`; the converter then spends three minutes writing 21 GiB before raising `Missing MoE
expert source layers`. The check catches that in under three seconds and quotes the
mismatch.

**Chat templates are a file operation, and it says so.** FreeToken has no
`--chat-template` flag — it loads the template through
`AutoTokenizer.from_pretrained(model_path)` — so overriding one means writing
`chat_template.jinja` into the checkpoint directory, where it takes precedence over the
`chat_template` key in `tokenizer_config.json`. ft-man never overwrites: the checkpoint's
own template is moved aside, a marker records what was applied and from where, and `u`
restores the original exactly (removing the file outright when the checkpoint never had
one). A checkpoint and its FTW build are written together, because each carries its own
tokenizer files and an override applied to only one would silently miss whichever you
serve.

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
host = "0.0.0.0"       # also the default `ft serve --host`
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

[convert]
# Ask FreeToken what it makes of a checkpoint before converting it.
preflight = true

[templates]
# Repos offered when fetching chat templates. Any repo holding .jinja files works.
sources = ["peculiar-ragdoll/Qwen-Sharp-Chat-Templates"]
# Render the template against the model's real tokenizer before writing it in.
preflight = true

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

## Using a different chat template

The **Templates** tab (`4`) fetches `.jinja` templates from any Hugging Face repo and
applies them to a checkpoint:

1. `r` → enter a repo (`peculiar-ragdoll/Qwen-Sharp-Chat-Templates` is the shipped
   default) → Enter to list its templates.
2. `f` on one to fetch it into the local store under `~/.local/state/ft-man/templates`.
3. Select the model on the **Models** tab, come back, and press `a`. The confirmation
   names every directory that will be written.
4. `v` renders the template against that model's real tokenizer — with a system prompt, a
   tool definition and a tool result — and reports the failure if it does not. This runs
   automatically before an apply unless you turn `templates.preflight` off; a template
   that fails to render breaks every request the engine serves, so it is worth the few
   seconds.
5. `u` restores the checkpoint's own template.

Because the engine reads its template when the model loads, **restart the engine** for a
change to take effect. The Models tab shows each checkpoint's current template, so an
override is never invisible.

Templates that take `chat_template_kwargs` (the Qwen-Sharp ones accept
`enable_thinking`, `tool_call_format`, `max_tool_arg_chars` and others) read them from
the request body — FreeToken passes `chat_template_kwargs` straight through from the
OpenAI and Anthropic APIs.

## Development

```bash
cargo test        # 134 tests, including render and input sweeps across five terminal sizes
cargo clippy --all-targets
cargo fmt
```

The render tests draw every view at 40×12 through 200×60, in every theme, empty and
populated, with each overlay open — and the input tests press every printable and
navigation key on every tab. A TUI that panics mid-draw corrupts the terminal, so that is
the failure mode most worth spending tests on.

## Notes

- [docs/freetoken-compressed-tensors-moe.md](docs/freetoken-compressed-tensors-moe.md) —
  why FreeToken cannot load compressed-tensors NVFP4 MoE checkpoints today, what was
  verified, and a sketch of the upstream fix.

## License

Apache-2.0, matching FreeToken.
