# FreeToken cannot load compressed-tensors NVFP4 MoE checkpoints (Qwen3.5-MoE family)

Handoff for a future session. Everything below marked **VERIFIED** was executed and
observed; everything marked **UNVERIFIED** is inference that still needs testing.

- **Written:** 2026-09-05 (revised the same day after a review pass — see §8)
- **FreeToken:** v0.1.2, commit `af71ba43206e124f5ff6419b47ee36c6e9981078` (2026-09-03)
- **Fork to work in:** `git@github.com:jlbyh2o/FreeToken.git` (`origin`), with
  `upstream` = `https://github.com/FlashML-org/FreeToken.git`, push-disabled
- **Goal:** decide whether this is repairable and, if so, repair it

---

## 1. Symptom

`ft checkpoint` (and `ft serve`) on a compressed-tensors NVFP4 Qwen3.5-MoE checkpoint
runs for ~3 minutes, writes ~21 GiB, and then dies:

```
ValueError: Missing MoE expert source layers:
  {'gate_up': [0..39], 'down': [0..39]}
```

Reproduced on `~/models/Ornith-1.5-35B-A3B-NVFP4` (23.3 GiB,
`Qwen3_5MoeForConditionalGeneration`, 256 experts x 40 layers). **VERIFIED** — twice, in
`~/.local/state/ft-man/logs/convert-20260905-2*.log` on the server.

The same wall was seen from the Hub on `Qwen/Qwen3.6-35B-A3B-FP8`, i.e. it is **not**
specific to one community build. **VERIFIED** (config inspection only, not a download.)

---

## 2. Root cause

### 2a. The expert-quantization detector never looks at `format`

There are two detectors. The general one understands llm-compressor exports; the
Qwen3.5-MoE family has its own, written for nvidia/modelopt, and that is the one used.

| detector | reads | result on this checkpoint |
|---|---|---|
| `python/freetoken/models/config.py:21` `detect_expert_quant` | `quant_algo`, `quant_method`, **`format`**, `config_groups` | `nvfp4` |
| `python/freetoken/models/config.py:54` `detect_compressed_tensors_nvfp4` | `config_groups` shape | `True` |
| `python/freetoken/models/qwen3_5_moe/config.py:42` `_expert_quant` **(the one used)** | `quant_algo`, `quant_method`, `quantized_layers` — **never `format`** | `none` |

`parse_config` (`qwen3_5_moe/config.py:167-169`) calls `_fp8_block_quant` then falls back
to the family-local `_expert_quant`. The checkpoint declares:

```json
"quantization_config": { "quant_method": "compressed-tensors",
                         "format": "nvfp4-pack-quantized" }
```

`"compressed-tensors"` contains neither `fp4` nor `mixed`, so `_expert_quant` returns
`"none"`. **VERIFIED** by calling all three functions directly against the real config.

With `expert_quant == "none"`, `python/freetoken/moe/expert_banks.py:298` dispatches through
`_PROVIDERS["none"]` to `_bf16_banks` (`expert_banks.py:65`), which looks for unpacked bf16
expert tensors and finds none for any of the 40 layers. The raise itself is one level down,
in `python/freetoken/models/loader.py:419` — grepping `expert_banks.py` for the message
finds nothing. **VERIFIED**

> Note: `parse_config` *does* call `_compressed_tensors_nvfp4` at
> `qwen3_5_moe/config.py:185`, but that branch only sets `attn_quant`, `dense_quant` and
> `lm_head_quant`. It never touches `expert_quant`.

### 2b. Nothing in the config can work around it

Patching `quant_algo: "NVFP4"` into a copy of the config **does** flip the detector to
`expert_quant=nvfp4`, and the conversion then gets past it and fails deeper:

```
KeyError: (0, 0, 'down_proj')
  python/freetoken/models/nvfp4_banks.py:185
      global_scale = globals_map[(layer, expert, proj)]
```

Because the NVFP4 bank loader was given the **modelopt** spec, whose `key_pattern` matches
`.weight` / `.weight_scale` / `.weight_scale_2`, while the checkpoint stores
`.weight_packed` / `.weight_scale` / `.weight_global_scale`.

**VERIFIED** — run on the server against a symlink farm with a patched `config.json`; the
real model directory was never modified.

### 2c. The dense pass also has no MoE branch

`qwen3_5_moe/weight.py:565` `_iter_weights_compressed_tensors` says in its own docstring:

> *"Dense pass for a compressed-tensors NVFP4 checkpoint (e.g. Qwen3.6-27B). … The model is
> dense (no routed experts), so there is no experts pass."*

That explains the ~21 GiB dense phase: with the compressed-tensors branch taken at
`weight.py:182`, the packed expert tensors are emitted as ordinary dense weights instead of
being excluded for the offload bank pass. **VERIFIED** — observed byte counts, and confirmed
in the code since: the function's only early exit is `if not include_non_moe: return`
(`weight.py:579`), so the offload call (`include_non_moe=True, include_moe_experts=False`)
runs the full loop; and `_rename` (`weight.py:134`) drops `mtp.`/`visual.` prefixes but never
routed experts, so the expert tensors reach the emit path.

---

## 3. Why this looks small to fix

FreeToken already contains every piece; they are simply not wired together for this family.
GLM-5-Next solved the identical problem:

```python
# python/freetoken/models/glm5_next/weight.py:74
_NVFP4_CT_SOURCE_SPEC = Nvfp4ExpertSourceSpec(
    key_pattern=re.compile(
        r"^model\.language_model\.layers\.(?P<layer>\d+)\.mlp\.experts\.(?P<expert>\d+)\."
        r"(?P<proj>gate_proj|up_proj|down_proj)\."
        r"(?P<kind>weight_packed|weight_global_scale|weight_scale)$"
    ),
    proj_to_role={"gate_proj": "gate", "up_proj": "up", "down_proj": "down"},
    layer_to_bank=_layer_to_bank,
    desc="GLM-5.3 NVFP4 experts (compressed-tensors)",
    kind_map={"weight_packed": "weight", "weight_global_scale": "weight_scale_2"},
    global_reciprocal=True,
)

# python/freetoken/models/glm5_next/weight.py:88
def _select_expert_source_spec(model_path: str) -> Nvfp4ExpertSourceSpec:
    ...
    return _NVFP4_CT_SOURCE_SPEC if method == "compressed-tensors" else _NVFP4_SOURCE_SPEC
```

Compare the two families' key patterns — **the prefixes are byte-identical**, only the
`kind` alternation differs:

```
qwen3_5_moe/weight.py:40   ...experts\.(?P<expert>\d+)\.(?P<proj>...)\.(?P<kind>weight|weight_scale|weight_scale_2)$
glm5_next/weight.py:76     ...experts\.(?P<expert>\d+)\.(?P<proj>...)\.(?P<kind>weight_packed|weight_global_scale|weight_scale)$
```

And GLM's pattern matches the Ornith checkpoint's tensor names verbatim — confirmed against
its `model.safetensors.index.json`, which holds
`model.language_model.layers.N.mlp.experts.N.{gate,up,down}_proj.{weight_packed,weight_scale,weight_global_scale,input_global_scale}`
at 10240 entries each (256 experts x 40 layers). **VERIFIED**

`kind_map` and `global_reciprocal` are generic fields on `Nvfp4ExpertSourceSpec`
(`nvfp4_banks.py:20-30`), not GLM-specific.

---

## 4. Proposed fix — three parts, all **UNVERIFIED**

1. **`qwen3_5_moe/config.py::_expert_quant`** — recognize a compressed-tensors NVFP4
   export by reusing the already-imported `_compressed_tensors_nvfp4` (in scope at
   `config.py:68`) and returning `"nvfp4"`.

   Do **not** simply fall back to the shared `models/config.py::detect_expert_quant`. Its
   own docstring says "Models with mixed-precision configs (e.g. qwen3_5_moe) need their
   own detector", and it returns the lowercased algo string for anything it does not
   recognize as NVFP4: a modelopt `MIXED_PRECISION` export yields `"mixed_precision"` and a
   compressed-tensors FP8 export yields `"compressed-tensors"`, neither of which is a key in
   `moe/expert_banks.py::_PROVIDERS`. That trades the missing-layers error for
   "no expert-bank provider for expert_quant=..." on checkpoints the family detector
   deliberately answers `"none"` for.
2. **`qwen3_5_moe/weight.py`** — add a `_NVFP4_CT_SOURCE_SPEC` mirroring GLM's, with
   Qwen's `layer_to_bank=lambda layer, config: layer`, plus a `_select_expert_source_spec`
   like GLM's; use it at `weight.py:1068-1071` where `_NVFP4_SOURCE_SPEC` is passed to
   `load_nvfp4_expert_source_banks` (and the parallel variant near `weight.py:1082`).
3. **`qwen3_5_moe/weight.py:182`** — the compressed-tensors dense branch must exclude
   routed experts when `include_moe_experts=False` (offload), instead of emitting them as
   dense weights. Check `_iter_weights_compressed_tensors` against a MoE checkpoint; it
   currently early-returns on `not include_non_moe` and otherwise assumes no experts exist.

**Scope: this plan covers the offload path only.** Parts 2 and 3 fix the expert *banks*
(`--moe-backend offload`, and the bank-fed part of `hybrid`/`cpu`). They do not address a
resident load, where `include_moe_experts=True` sends the experts through the dense pass:
that path needs them in the pre-fused `experts.gate_up_proj` / `experts.down_proj` layout
`_PACKED_EXPERT_PATTERN` expects (`qwen3_5_moe/weight.py:31`), and a compressed-tensors
checkpoint stores them per-expert and un-fused. ft-man offers `auto`/`offload`/`hybrid`/
`cpu`/`fused` (`src/knobs.rs:172`), so a user can reach the unfixed path. Decide explicitly
whether to fuse-on-load for it or to reject `fused` for these checkpoints with a clear
message. **UNVERIFIED** — reasoned from the key patterns, not run.

**Open questions to answer before or while implementing:**
- Is `global_reciprocal=True` correct for this checkpoint, or is that GLM-specific? It
  controls whether the quant-side global scale is inverted at ingest
  (`nvfp4_banks.py` `_ingest_global`).
- `input_global_scale` is present in the checkpoint but deliberately unmatched by GLM's
  pattern (its comment says the routed-expert paths are W4A16 and never quantize
  activations). Confirm that holds for Qwen3.5-MoE too.
- Does the MTP head (`model_mtp.safetensors`, `mtp.layers.N.mlp.experts.*`) need
  excluding? Qwen's comment at `weight.py:35-38` says the `model.language_model.` anchor
  already does that.
- Does `attn_quant`/`dense_quant` being forced to `nvfp4` at `config.py:185` remain right
  once `expert_quant` is also `nvfp4`?

---

## 5. How to reproduce and test

Test checkpoint on the server: `~/models/Ornith-1.5-35B-A3B-NVFP4` (23.3 GiB, already
downloaded). Note it currently carries an ft-man chat-template override
(`chat_template.jinja` + `.ft-man-original` + `.ft-man-template.json`) — harmless here.

**Cheap check (~3s, no GPU, no weights)** — does the detector resolve the experts?

```bash
~/FreeToken/.venv/bin/python -c '
from freetoken.utils import cached_load_hf_config
from freetoken.models.config import detect_expert_quant, detect_compressed_tensors_nvfp4
from freetoken.models.qwen3_5_moe.config import _expert_quant
cfg = cached_load_hf_config("/home/jeremy/models/Ornith-1.5-35B-A3B-NVFP4")
print("shared :", detect_expert_quant(cfg))            # nvfp4
print("ct     :", detect_compressed_tensors_nvfp4(cfg))# True
print("family :", _expert_quant(cfg))                  # none  <-- the bug
'
```

ft-man wraps the same idea: `ft-man` → Jobs tab → convert runs a preflight that prints the
resolved `expert_quant` in seconds. `src/ft/preflight.rs::CONVERT_SCRIPT` is the script.

**Full check (~3 min, needs the GPU idle)**

```bash
FREETOKEN_CONVERT_PROGRESS=1 ~/FreeToken/.venv/bin/ft checkpoint \
  --model ~/models/Ornith-1.5-35B-A3B-NVFP4 \
  --out /tmp/ornith-test-ftw --moe-backend offload
```

Success criteria, in order:
1. `FTCONVERT dense …` should now total *far less* than 21 GiB (experts excluded).
2. An `FTCONVERT experts …` phase should appear at all — it never has yet.
3. `wrote FTW checkpoint -> …` with a non-`None` `quant_format`.
4. Then `ft serve --model /tmp/ornith-test-ftw --moe-backend offload` and check output
   quality — a silently wrong scale convention would show up as garbage tokens, not a
   crash. **This is the step most likely to reveal a subtle error.**

Steps 1-4 exercise `offload` only. If part 3's scope decision above includes the resident
path, repeat step 4 with `--moe-backend fused` (16 GiB of VRAM will not hold 35B at FP4, so
expect to test it on `hybrid` or accept an explicit rejection instead).

Clean up `/tmp/ornith-test-ftw` afterwards (~21 GiB). Server had 82 GB free at handoff.

---

## 6. Environment

- Server `10.242.20.111` (`ssh jeremy@…`), Debian 13 LXC named `freetoken`,
  RTX 5070 Ti 16 GiB, 40 GiB RAM, Ryzen 7 5700X (8 threads, **AVX2, no AVX-512**)
- FreeToken source checkout at `~/FreeToken`, venv at `~/FreeToken/.venv`, still pointed at
  FlashML upstream — **repoint it at the fork before editing there**
- `vendor-freetoken/` in this repo is the laptop-side clone wired to the fork; it is
  gitignored by ft-man and is a reference copy, not the one the server runs
- ft-man itself is at `~/.local/bin/ft-man` on the server

## 7. Where ft-man encodes this finding

If the upstream fix lands, these need revisiting:

- `src/compat.rs::unresolvable_experts` — mirrors the buggy family detector to warn on the
  Hub before downloading. Its doc comment records the same evidence.
- `src/ft/preflight.rs::CONVERT_SCRIPT` — the convert preflight, whose doc comment explains
  the same two-detector split and whose `WARN` branch fires on
  `is_moe and expert_quant == "none"` with a declared quant.
- `src/compat.rs` tests `an_llm_compressor_moe_export_is_caught_with_the_real_reason` and
  `the_family_that_can_read_compressed_tensors_is_not_flagged`.
- `README.md`, the "A doomed conversion fails in seconds" paragraph.

## 8. A correction worth not repeating

My first explanation was **wrong**: I claimed the multimodal wrapper keeps the language
model under `text_config` while `quantization_config` stays top-level, so the detector read
the nested config and found nothing. Copying `quantization_config` into `text_config`
changed nothing — `parse_config` passes the top-level config regardless. Test the claim
before encoding it in a user-facing message.

The correction itself then came up short, which is the second lesson. Commit `60b80b6`
rewrote `README.md`, `src/compat.rs` and `src/ui/smoke.rs`, and the first draft of this
handoff recorded the theory as fully retracted — but a verbatim copy survived in
`src/ft/preflight.rs`'s doc comment above `CONVERT_SCRIPT` and shipped for two more commits,
because §7's list of places encoding the finding did not mention that file. It was caught in
a review pass and corrected on 2026-09-05. When retracting a claim, grep the tree for its
distinctive words (`text_config` here) rather than trusting a list of the places you
remember writing it.
