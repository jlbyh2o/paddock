# KV geometry: pricing the context before the download

**Status: built.** Proposed and implemented 2026-09-14. The design lives in
`src/compat.rs`; the reasoning has moved to
[design-notes.md](design-notes.md#it-prices-the-cache-before-the-download-not-after) and the
wire shape to [web-api.md](web-api.md#compatkv--what-the-cache-costs). This file is kept as
the working record — what was proposed, what survived contact with the code, and what did
not.

Section 9 lists the four places the proposal was wrong. Read it before trusting anything
above it.

The motivating question: "can this server run `IFM/K2-Horizon-MoVA-36B-A4B`?" It is a
36B-A4B MoE, the same class as the Qwen3.6-35B-A3B this machine already serves at 256k, and
its weights offload the same way. It cannot serve 8k of context on a 5070 Ti, and nothing
paddock currently computes would say so.

---

## 1. The gap

`compat.rs` prices three things against the hardware: download size against free disk,
download size against host RAM (offloaded expert banks), and download size against VRAM
(dense models have nowhere to spill). All three are about **weights**.

The KV cache is never priced. `Report` already carries `num_layers` and `context`, and
`evaluate()` already reads both out of the config — they reach `summary()` as display
fields and no further.

On a 16 GiB card that is the wrong thing to leave out. Weights offload; the cache cannot.
A model whose weights fit comfortably can still be unable to hold a tenth of its
advertised context, and the current report calls that repo supported.

Two smaller things fall out of the same reading:

- The hub path passes `download_bytes = 0` (`actions.rs:1021`, deliberately — the comment
  says it keeps the report to what the config alone can say), so today *none* of the three
  hardware notes ever fire from the Hub tab. Only architecture and quantization do.
- design-notes already has **"It notices when you are serving a fraction of your
  context"**. That is this same insight, observed after launch from a running engine's
  `/v1/cache/status`. This note is the half that can be answered from a config alone, half
  an hour earlier, before the transfer.

## 2. Why the cache is a per-layer cost

The common intuition is that the KV cache is one buffer that the conversation accumulates
into. It is not, and the difference is the whole feature.

A token is not attended to once. It is attended to once per layer, by that layer's own
attention, over that layer's own inputs — layer 30 reads layer 29's output, not the text.
So each layer keeps its own key and value vector for every token it has seen, and layer
12's are not substitutable for layer 30's: they are representations at different levels of
abstraction. N layers means N independent caches, each one row per token.

```
bytes/token  =  2 (K and V)  x  layers that keep a growing cache  x  KV width  x  bytes/elem
```

Nothing in that expression is parameter count. Two 35B MoE checkpoints can differ by 10x
here, and do: 20 KiB/token against 192 KiB/token, verified in section 7.

The multiplier that matters most is the first one — **how many layers keep a growing cache
at all**, which is usually fewer than `num_hidden_layers` and is the number paddock has to
derive rather than read.

## 3. The four shapes

`layer_types` (or its absence) decides which of these each layer is.

| Shape | Recognized by | Per-token cost | Grows with context? |
|---|---|---|---|
| Full attention (MHA/GQA) | `full_attention`, or no `layer_types` and no `sliding_window` | `2 x kv_heads x head_dim x bytes` | yes |
| MLA (DeepSeek, GLM) | `kv_lora_rank` present | `(kv_lora_rank + qk_rope_head_dim) x bytes`, one latent, not per head | yes, but cheaply |
| Sliding window | `sliding_attention`, or `sliding_window` with `use_sliding_window` | same as full, but capped at `min(ctx, window)` | no, flat after the window |
| Linear / stateful | `linear_attention`, `kda`, `mamba` | a fixed state matrix per layer per **sequence** | no, flat from token 1 |

`head_dim` falls back to `hidden_size / num_attention_heads`; `num_key_value_heads` falls
back to `num_attention_heads`. Multimodal wrappers keep all of it under `text_config`,
which `evaluate()`'s existing `field()` helper already handles.

The last row is the one worth stating in the UI, because it is the least intuitive: a
linear-attention layer folds each token into a fixed-size running state and overwrites it.
Qwen3.6's 30 linear layers cost 2 MiB each per sequence — the same at one token as at
256k. A sliding-window layer does keep rows, but forgets everything past its window.
Both escape the "grows forever" trap by different means, and a model with full attention on
every layer escapes it not at all.

## 4. What to compute, and what to say

Two derived numbers. The first is the honest unit:

```
kv_bytes_per_token  =  sum over growing layers of (per-layer row)
```

The second is the one a reader actually wants, and paddock is unusually well placed to
give it because it already has NVML and the weight size:

```
max_servable_context  =  (vram - weights - expert_slot_floor - state_pool) / kv_bytes_per_token
```

Report it against what the repo advertises, because the gap is the finding:

> serves about **38k** of its advertised **512k** on this card (192 KiB/token, 48 of 48
> layers cache)

Suggested levels, consistent with the existing `Level` ladder:

- **Info** — the model can reach its advertised ceiling here. Say the per-token cost anyway;
  it is what makes the next comparison possible.
- **Caution** — it reaches somewhere between 25% and 100% of its ceiling.
- **Blocker** — it cannot hold some floor worth having. A fixed floor is the wrong shape;
  better to take it from `config.library.min_context` or similar, defaulted to 32k, so the
  verdict answers *this reader's* requirement rather than a number paddock invented.

`summary()` should carry the per-token figure, since it is the one field that makes two
candidate repos comparable at a glance: `... · 256k ctx · 20 KiB/tok`.

KV is **bf16 today** — FreeToken has no KV dtype flag (**VERIFIED**: no `kv_cache_dtype` in
`server/args.py` or `engine/config.py` as of `84d236c`). The fp8 work in flight halves every
number below. The estimator should price bf16 and not pretend otherwise; when a dtype flag
lands, this becomes a factor of `bytes/elem` and nothing else changes.

## 5. Where it goes

| # | File | Change |
|---|---|---|
| 1 | `src/compat.rs` | new `kv` module or section: `fn kv_geometry(config: &Value) -> Option<KvGeometry>`, returning `{growing_layers, flat_layers, windowed_layers, bytes_per_token, window, shape}` |
| 2 | `src/compat.rs` | `Report` gains `kv: Option<KvGeometry>` and `max_servable_context: Option<u64>`; `summary()` appends the per-token cost |
| 3 | `src/compat.rs` | `evaluate()` emits the Info/Caution/Blocker note. Needs `hw.vram_bytes`, which it already takes, and the weight size — which means the Hub path should stop passing `download_bytes = 0` |
| 4 | `src/actions.rs:1021` | pass the real listing size; the caller has it (`check_compatibility`'s docstring already anticipates this) |
| 5 | `src/ui/views/hub.rs` | the note renders through the existing `Level` glyphs; no new widget |
| 6 | `src/web/snapshot.rs` | `Report` is already `Serialize`; the new fields ride along. `docs/web-api.md` needs the shape documented |
| 7 | `src/plan.rs` | the planner already prices KV against `cache_budget_bytes` from a **running** engine. Geometry from the config is the same arithmetic without the measurement, so the pre-download estimate and the post-launch plan should agree; worth a test that they do on a model that has been served |

Section 7's fixtures belong in `#[cfg(test)] mod tests` beside the existing ones, in the
same style — a real checkpoint per case, named for the shape it exercises.

`scripts/kv-geometry.py` implements sections 3 and 4 against a live or local `config.json`
and produced every number in section 7. It exists so the port has something to check
against, and so the question is answerable before the port lands; it is a development tool,
not something paddock shells out to.

## 6. What this cannot know

The house rule is that a clean report is not a promise. Specifically:

- **It reads what the config declares.** A checkpoint whose remote code caches differently
  than its config implies will be mispriced. K2-Horizon happens to be honest.
- **Paged allocation rounds up.** FreeToken serves `num_pages * page_size`, so the real
  ceiling is a page-granular floor of this estimate, not the estimate.
- **The expert slot cache competes for the same budget**, and how much it wants is only
  knowable from a served run (`costs.json`). Before a model has been served, the honest
  output is a range or a "with N GiB left for experts" qualifier — the same discipline
  design-notes already applies to the unpriced split.
- **`--max-running-requests` multiplies the state pool** on hybrid-linear models, and on
  those models the state pool is a sibling of the KV pool, not part of it. Already
  described in design-notes; the estimator should reuse whatever prices it.
- **It says nothing about quality.** A model that fits is not thereby a model worth running.

## 7. Fixtures — **VERIFIED** 2026-09-14

Live `config.json` of each repo, bf16 KV, context 256k where the model allows it.

| Repo | Layers | Growing | Shape | Per token | 256k KV |
|---|---|---|---|---|---|
| `Qwen/Qwen3.6-35B-A3B` | 40 | **10** | 30 linear + 10 full, 2 kv x 256 | 20 KiB | 5.00 GiB |
| `RedHatAI/GLM-5.3-Flash-NVFP4` | 45 | **11** | 34 linear + 11 MLA, latent 512 | 11 KiB | 2.75 GiB |
| `meta-models/Muse-Glimmer-30B` | 52 | **13** | 39 SWA(2048) + 13 full, 2 kv x 128 | 13 KiB | 3.33 GiB + 0.08 capped |
| `google/gemma-4-26B-A4B-it` | 30 | **5** | 25 SWA(1024) + 5 full, 8 kv x 256 | 40 KiB | 10.20 GiB + 0.20 capped |
| `openai/gpt-oss-120b` | 36 | **18** | 18 SWA(128) + 18 full, 8 kv x 64 | 36 KiB | 9.00 GiB |
| `Qwen/Qwen3.8-27B` | 64 | **16** | 48 linear + 16 full, 4 kv x 256 | 64 KiB | 16.00 GiB |
| `IFM/K2-Horizon-MoVA-36B-A4B` | 48 | **48** | all full, 8 kv x 128 | 192 KiB | 48.00 GiB |
| `nvidia/MiniMax-M2.5-NVFP4` | 62 | **62** | all full, 8 kv x 128 | 248 KiB | 62.00 GiB |

Read the "Growing" column, not the "Layers" column. That is the feature.

## 8. The case that motivated it

`IFM/K2-Horizon-MoVA-36B-A4B` is today caught by the architecture check — `k2_horizon` is
not in FreeToken's registry, so the verdict is already `Unsupported` and the KV wall behind
it never has to be reached.

That is luck, and it is worth writing down because it expires. The architecture is a
tractable port: ordinary GQA, a sigmoid-bias router GLM-4 already matches, and an output
gate three families already implement. The day a `k2_horizon` loader lands upstream, this
repo's verdict flips to `Supported` on a 16 GiB card, and the thing that actually makes it
unservable — 48 of 48 layers caching at 192 KiB/token, 48 GiB at 256k, 3 GiB at 16k — is a
number nothing in paddock computes.

The same reasoning generalizes past this one checkpoint. Architecture support is a moving
target that upstream keeps widening; KV geometry is fixed by the checkpoint and does not
improve. Of the two walls, the one paddock checks today is the one that keeps falling down.

---

## 9. What the implementation changed — **2026-09-14**

Four things in sections 1–8 did not survive being built. They are left in place above
rather than edited out, because the corrections are more useful than a tidy document.

### 9.1 Step 4's premise was false: the caller does not have the size

Section 5 says "pass the real listing size; the caller has it." It does not.
`open_repo` spawns the `hub.info` fetch and calls `check_compatibility` in the same breath
(`actions.rs`), so the two race and `hub_view.files` is empty when the check is made.
Worse, a multi-variant repo deliberately pre-ticks nothing, so there is often no download
size in principle rather than not yet.

The fix is bigger and better than the proposal: the report is not the answer to a fetch,
it is a **function of state that keeps moving**. `Message::Compatibility` now carries the
raw `config.json`, `HubView` keeps it, and `App::reprice_compat()` re-runs the arithmetic —
which is pure and costs nothing — whenever any input changes: the config arriving, the
listing arriving, the architecture registry arriving, or a reader picking a quantization.
That last one is the point. An NVFP4 build and a bf16 build of the same checkpoint differ
fourfold in resident weights, so the context a card can hold moves with the choice, and a
one-shot report could only ever have described one of them.

It also fixed a pre-existing bug the proposal never noticed: a verdict reached before
FreeToken's registry answered said architecture support was "unverified" for ever.

### 9.2 The blocker as specified would almost never have fired

Section 8 says K2-Horizon "cannot serve 8k of context on a 5070 Ti". That is false. At
192 KiB a token, 8k of KV is 1.5 GiB, and a 16 GiB card has about 14.4 GiB to play with.
The checkpoint reaches something like 77k before a single byte of weights is paid for.

The true finding is the *share*, not the floor: about a sixth of what the repo advertises,
and that is the upper bound rather than the plan. The blocker is still worth having, but it
is rare by design, and it is judged against the reader's own `--kv-reserve-tokens` rather
than a fixed 8k — so someone who serves 256k conversations gets the blocker on this
checkpoint and someone who serves 8k ones does not. Section 4 asked for
`config.library.min_context`; that key does not exist and did not need to, because
`--kv-reserve-tokens` is already paddock's record of the context an operator wants.

### 9.3 The Caution band as specified would have fired on everything

Section 4 proposes Caution for anything between 25% and 100% of the advertised ceiling.
Modern checkpoints ship 256k and 1M ceilings that essentially nothing on a consumer card
reaches, so that rule puts a warning glyph on almost every repo, and a caution that is
always on is one nobody reads. Implemented the other way round: the gap is **always named**,
at Info, and only a gap below a quarter of the ceiling warns.

### 9.4 Step 7 wanted a test that would have been flaky

Section 5 wants the pre-download estimate and `plan.rs`'s post-launch plan to agree, and a
test asserting it. They will not agree, and `plan.rs` says why in its own doc comment: its
`Costs` come only from a live engine precisely because reconstructing them from
`config.json` means reimplementing FreeToken's cost model and being quietly wrong when it
changes. Page rounding, the TP shard, the fixed non-paged cache and the state pool all sit
between the two numbers.

That stance is right and is untouched. These are two different classes of number: `plan.rs`
refuses to *derive* a cost when it has a *measured* one, while the Hub check has no
measurement available at all, where an estimate is strictly better than the silence it
replaced. They are kept structurally separate, the estimate is labeled as one, and no test
asserts they match.

### What was right

Sections 2, 3 and 7. The layer taxonomy is the whole feature and needed no change; all
eight fixtures reproduce exactly and are now `the_fixture_checkpoints_price_as_measured`.
Section 6's list of what this cannot know is unchanged and is why `context_is_upper_bound`
exists. Section 8's closing argument — that architecture support keeps widening while KV
geometry never improves — is the reason the feature was worth building, and it is now in
design-notes.

`scripts/kv-geometry.py` stays. It answered the question before the port existed and it
remains the reference the Rust is checked against; every number in section 7 came from it
and the Rust reproduces all of them.
