# Design notes

Why ft-man behaves the way it does. Most of this is about FreeToken's defaults and the
places where the right thing to do is not the obvious one — the reasoning is here rather
than in the README so that the README can stay a page about getting started.

If you are looking for the HTTP API the browser UI speaks, that is
[web-api.md](web-api.md); the architecture behind it is [web-ui.md](web-ui.md).

### It owns the engine, and it lets go

`ft serve` is started in its own session, its
output is teed to a file under `~/.local/state/ft-man/logs`, and `{pid, starttime, args}`
is recorded so a later `ft-man` re-attaches to a serve that is still running. Quitting
`ft-man` deliberately leaves the engine up — a loaded 200 GiB model should not die because
a TUI closed. Only a stop you asked for stops it.

### Progress is real, not a spinner

`ft checkpoint` and `ft bench bw` both emit a
machine-readable line protocol when asked (`FTCONVERT`, `FTBENCH`); `ft-man` sets those
env vars and parses them, so a two-hour conversion shows actual bytes against actual
totals.

### The knob table is the documentation

Every flag carries its type, range, default,
help text and mutual exclusions in one schema. Setting `--moe-cache-size` clears
`--moe-cache-rate` for you, because the engine would reject the pair.

### It notices when you are serving a fraction of your context

FreeToken sizes the KV
cache from whatever VRAM the expert cache leaves it — `--moe-cache-auto` reserves
`--kv-reserve-tokens` (8192 by default), lets the experts take the rest, and hands KV the
remainder. The engine then serves `min(model_ceiling, num_pages * page_size)` without
logging that it clamped, while `/v1/models` keeps advertising the checkpoint's full
ceiling on purpose. So a 256k model can be answering with 8k and nothing says so. The
Dashboard puts both numbers on one line and says which is real.

### And it can plan the fix

Press `a` on the Serve tab. ft-man prices the cache split
from what the engine measured — `cache_budget_bytes` and the per-unit KV and expert costs
from `/v1/cache/status` — and works out the `--kv-reserve-tokens` that buys back the
context, what it costs in expert slots, and whether the card can reach the ceiling at all.
It also makes the two calls `auto` will not: `--moe-strategy fused` when every expert
demonstrably fits in VRAM alongside full-context KV (the engine refuses to guess, because
a wrong guess is an OOM at weight load; ft-man has NVML and the geometry, so it is
arithmetic), and hybrid-vs-offload from the `ft bench bw` profile. Every line says why,
with the numbers it used. `A` applies it to the configuration you were already editing;
nothing is changed until you press it.

Costs are only knowable from a running engine, so ft-man writes down what each serve
measured (`~/.local/state/ft-man/costs.json`) and plans the next launch of that model
exactly. Before a model has ever been served, the plan says the split is unpriced rather
than inventing one.

### It estimates prefix-cache reuse, and says that it is an estimate

FreeToken reports
the exact figure only in a completion's `usage.prompt_tokens_details` block, which ft-man
never sees — it polls the control plane rather than proxying model traffic, and neither
`/v1/stats` nor the request ring carries a cached-token count. So the Dashboard infers it:
time to first token is dominated by prefill, prefill only covers the uncached part of a
prompt, and the slowest request in the ring anchors the hardware's cold rate. A 70k prompt
reaching first token in 0.8 s on a card that prefills ~3k tokens/s did not prefill 70k
tokens. When the ring holds no spread between slow and fast requests the figure is withheld
rather than guessed, because a uniformly quick session and a quick GPU look identical from
outside.

### It prices the GDN state pool, which is easy to forget and expensive

On a
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

### It knows host RAM is spent too

The offload backends pin *every* expert in host RAM,
resident or not, so the host-side footprint is the whole model's experts rather than the
cache. ft-man reports when those banks dominate RAM, and a benched `hybrid` verdict yields
to a host with no headroom — hybrid's CPU executor needs working room on top of the banks,
and a faster backend that cannot allocate is not faster.

### A missing bandwidth profile is a speed ceiling with no symptom

Without
`~/.cache/freetoken/benchbw/<gpu-uuid>.json`, `--moe-strategy auto` can only ever resolve
to `offload`, and `--moe-hybrid-max-fetch auto` falls back to a fixed cap of 1 instead of
the bandwidth-matched split. `--doctor` and the plan both say so, and the benchmark is one
key on the Jobs tab.

### It says whether a repo can run here before you download it

Pressing Enter on a Hub
result fetches only `config.json` — one small request — and reports a verdict. The
architecture check is definitive: FreeToken's own registry is queried at startup, so it is
never a stale list baked into ft-man. On top of that it catches the multimodal
quantization split described below, and weighs the download against this machine's VRAM,
host RAM and free disk. A clean result is not a promise the model will serve; it means
none of the known walls are in the way.

### A doomed conversion fails in seconds, not minutes

Before running `ft checkpoint`,
ft-man resolves the checkpoint through FreeToken's own `EngineConfig` and compares what it
concluded against what the checkpoint declares. The case that motivated it: FreeToken's
expert-quantization detector for the Qwen3.5-MoE family reads `quant_algo`/`quant_method`
and never `format`, so an llm-compressor (`compressed-tensors`) NVFP4 export resolves to
`none`; the converter then spends three minutes writing 21 GiB before raising `Missing MoE
expert source layers`. The check catches that in under three seconds and quotes the
mismatch.

### You pick a quantization, not sixty files

A quantized GGUF repo is not a checkpoint,
it is a shelf of them: `unsloth/Qwen3.8-Flash-Next-GGUF` holds eleven builds of one model,
two multimodal projectors and a drawer of draft models, in sixty files. Presenting that as
sixty checkboxes asks the reader to know which shards belong together, which projector
matches and which directory is a draft model. So the Hub tab groups them and asks the one
question that matters — `UD-IQ3_XXS` or `Q8_0`? — with each option's real size beside it,
smallest first. Choosing one selects its shards and the shared tokenizer and config, adds
the projector (a vision model without one still loads and answers, it just silently cannot
see), and leaves the other ten builds and the draft models alone. Two layouts are
recognized: a directory per quantization, and a file per quantization. The raw file list is
still there behind Tab for the repo whose layout the grouping cannot express.

The same grouping runs over the cache on disk, so a downloaded build is listed the way it
was chosen, at its real size. That matters more than it sounds: the weights of one
quantization sit in a subdirectory, so a scan that only counted a snapshot's top level
reported an 82 GB model as the 862 MiB projector lying beside it.

### Nothing is served under a name FreeToken invented

`ft serve` defaults
`--served-model-name` to `os.path.basename(model_path)`, which for a cache snapshot is a
40-character commit sha and for a quantization directory is a bare `UD-IQ3_XXS` — neither
of which says which model it is. Worse, that name is the key ft-man remembers measured
cache costs under, so two quantizations sharing one name would price the second against the
first. ft-man therefore always passes the name explicitly, as `repo:variant`:
`unsloth/Qwen3.8-Flash-Next-GGUF:UD-IQ3_XXS`. FTW builds are named the same way, so
converting two quantizations of one repo cannot write both to one directory.

### The library is the Hugging Face cache, not a directory of its own

ft-man reads
`models--org--name/snapshots/<sha>/` out of `$HF_HOME/hub` (following `HF_HUB_CACHE` and
`HF_HOME` exactly as `huggingface_hub` does), so a checkpoint pulled by `hf download`,
`from_pretrained`, Unsloth or any other engine on the machine is already in the list —
under its repo id, not a commit sha, and at its real size, which means resolving the
symlinks a snapshot is made of. Downloads go back to the same place, delegated to `hf`
rather than reimplemented: the cache is blobs addressed by hash, a snapshot of symlinks per
revision, refs and `.incomplete` staging, and a second implementation that is subtly wrong
produces a cache every other tool quietly disagrees with.

Progress stays in bytes anyway. `hf` reports only a file count (`Fetching 12 files:  25%`),
which on a repo of two 40 GiB shards is a bar that sits at zero for an hour and then jumps
to done — so ft-man measures the cache instead: files resolved through the snapshot, plus
the `.incomplete` blob of whatever is in flight.

### What ft-man derives, it keeps out of the cache

FTW builds go to `library.ftw_dir`, and
a template override is never written into a snapshot. That tree belongs to
`huggingface_hub` — a directory it did not write is invisible to `hf cache scan` and at
risk from `hf cache delete`, and an FTW build has no repo id or revision for the cache to
file it under in the first place. It also is not private: on a dataset shared between
containers running different engines, editing a snapshot would silently change what all of
them serve. So an override applies to the FTW build, which is the copy the engine loads.
Names under `ftw_dir` are org-qualified for the same reason the cache's own are — two
organizations publishing the same model name is common, and a flat name merges them.

### Chat templates are a file operation, and it says so

FreeToken has no
`--chat-template` flag — it loads the template through
`AutoTokenizer.from_pretrained(model_path)` — so overriding one means writing
`chat_template.jinja` into the checkpoint directory, where it takes precedence over the
`chat_template` key in `tokenizer_config.json`. ft-man never overwrites: the checkpoint's
own template is moved aside, a marker records what was applied and from where, and `u`
restores the original exactly (removing the file outright when the checkpoint never had
one). A checkpoint and its FTW build are written together, because each carries its own
tokenizer files and an override applied to only one would silently miss whichever you
serve.

### It polls where the engine will actually be

If a profile pins port 1920, `ft-man`
polls 1920. A bind address of `0.0.0.0` is polled over loopback, because a wildcard is not
a destination.

### GPU exclusivity is enforced up front

A conversion or a benchmark needs the card, so
both refuse to start while an engine is running — with a message saying so, rather than a
CUDA OOM ten seconds in.
