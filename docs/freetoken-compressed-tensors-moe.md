# FreeToken and compressed-tensors NVFP4 MoE checkpoints (Qwen3.5-MoE family)

**Status: fixed upstream; this is history.** FreeToken #418/#427/#438 replaced every
family-local quantization detector with one `QuantConfig` layer that reads each module's
scheme by name, in either dialect. That covers this failure and more, so the local fork and
its patch were abandoned on 2026-09-11 and both ft-man and the container image now track
plain upstream. Kept because it records why ft-man's compatibility checks exist and what
they were built against.

Everything below marked **VERIFIED** was executed and observed on the server;
**UNVERIFIED** means reasoned but not run. All of it describes FreeToken as of `af71ba4`.

- **Written:** 2026-09-05. **Rewritten:** 2026-09-06, when the fix was implemented and
  tested. **Superseded:** 2026-09-11, when upstream's QuantConfig layer landed.
- **FreeToken then:** v0.1.2, upstream commit `af71ba43206e124f5ff6419b47ee36c6e9981078`
- **FreeToken now:** upstream `0ffd5c8`, no local changes
- **Fork (abandoned):** `git@github.com:jlbyh2o/FreeToken.git`; branch
  `fix/qwen3-5-moe-compressed-tensors-nvfp4-experts`
- **Test checkpoint:** `~/models/Ornith-1.5-35B-A3B-NVFP4` (23.3 GiB,
  `Qwen3_5MoeForConditionalGeneration`, 256 experts x 40 layers, `nvfp4-pack-quantized`)

---

## 1. What was broken

`ft checkpoint` on a compressed-tensors NVFP4 Qwen3.5-MoE checkpoint ran for ~3 minutes,
wrote ~21 GiB, then died:

```
ValueError: Missing MoE expert source layers: {'gate_up': [0..39], 'down': [0..39]}
```

The family-local expert detector (`qwen3_5_moe/config.py::_expert_quant`) reads `quant_algo`
and `quant_method` only. An llm-compressor export declares `quant_method:
"compressed-tensors"` and hides the FP4 geometry in `config_groups`, so the detector returned
`"none"`. `setup_offload_expert_banks` then dispatched to the bf16 provider, which looked for
unpacked bf16 expert tensors and found none. **VERIFIED**

Two consequences followed from the same wrong verdict: the routed experts were also emitted
as ordinary dense weights (hence the 21 GiB), and no experts pass ever ran.

---

## 2. The fix: five parts

The first three are the plan the previous session wrote. Parts 4 and 5 were **not** predicted
by it; both were found by actually loading the model, and neither shows up in a conversion.

| # | File | Change |
|---|---|---|
| 1 | `qwen3_5_moe/config.py` | `_expert_quant` recognizes a compressed-tensors NVFP4 export via the shared `detect_compressed_tensors_nvfp4`, gated on `num_experts > 0` so a dense Qwen3.6-27B keeps `"none"` |
| 2 | `qwen3_5_moe/weight.py` | `_NVFP4_CT_SOURCE_SPEC` (`weight_packed` -> `weight`, `weight_global_scale` -> `weight_scale_2`, `global_reciprocal=True`) plus `_select_expert_source_spec`, used at both bank-loader entry points |
| 3 | `qwen3_5_moe/weight.py` | the compressed-tensors dense pass skips routed experts, and raises a clear error for `--moe-backend fused` instead of silently dropping them |
| 4 | `qwen3_5_moe/weight.py` | `_CT_NVFP4_FUSE` gains the `.mlp.shared_expert.` layout. It only knew the bare `.mlp.` dense-MLP layout, so on a MoE checkpoint the shared expert's gate/up never fused into the `gate_up_proj` the model asks for |
| 5 | `models/config.py`, `qwen3_5_moe/config.py`, `qwen3_5_moe/model.py` | new `ModelConfig.linear_attn_quant`. This export's `ignore` list covers the **whole** GDN block, so `linear_attn.out_proj` is plain bf16 -- but `attn_quant="nvfp4"` had the model build it as an `Nvfp4DenseLinear`, and serving died on `KeyError: model.layers.0.linear_attn.out_proj.weight_scale` |

Part 5's verdict is read from the checkpoint's own `ignore` list rather than assumed.
Qwen3.6-27B ignores only `in_proj_*` and does pack `out_proj`, so both shapes are handled. A
partial export (some layers ignored, others not) raises rather than loading garbage into the
layers that disagree -- one verdict drives every GDN layer.

Answers to the previous session's open questions, all **VERIFIED** against the checkpoint:

- `global_reciprocal=True` is correct, and is a property of the format rather than
  GLM-specific. `weight_scale` maxes at exactly 448.0 (the fp8-e4m3 max) and
  `weight_global_scale` is 31360 for layer 0 expert 0, giving `2688/31360 = 0.0857` as the
  implied weight amax -- i.e. the stored global is the quant-side `448*6/amax`.
- `input_global_scale` is present but must stay unmatched; the routed-expert path is W4A16.
- The MTP head needs no extra exclusion: its 785 tensors sit under a top-level `mtp.` prefix,
  outside the `model.language_model.` anchor.
- `attn_quant`/`dense_quant`/`lm_head_quant` are unchanged by the fix
  (`nvfp4`/`nvfp4`/`none`); only the new `linear_attn_quant` differs.

---

## 3. What was verified

**Conversion** (`ft checkpoint --moe-backend offload`): succeeds in **64.8s**, where it
previously died at ~3 minutes. **VERIFIED**

```
wrote FTW checkpoint -> ...
  tensors: 663 weight + 240 experts_bank
  FTW: 20.96 GiB across 3 shard(s)
  quant_format: nvfp4  fingerprint=e98c76ec5d17516c
```

All four of the previous session's success criteria met: the dense pass is 4.03 GiB rather
than 21 GiB, a `Loading Qwen3.5 NVFP4 experts (compressed-tensors)` phase runs for the first
time, and `quant_format` is non-null.

**Expert key coverage.** Every routed-expert key the dense pass skips is claimed by the
spec: 30720 each of `weight_packed`/`weight_scale`/`weight_global_scale`, giving exactly
`L*E*6 = 61440` placements and `L*E*3 = 30720` globals across layers 0-39. The only
unmatched keys are `input_global_scale` (by design) and the MTP head's. **VERIFIED**

**Bank contents against the source.** For four (layer, expert) pairs spanning the model --
(0,0), (0,255), (17,42), (39,9) -- the converted banks were read straight out of the FTW
shards and compared with the checkpoint: packed weights byte-identical and in the right rows
(gate -> `[:I]`, up -> `[I:]`, down -> its own bank), fp8 block scales byte-identical, and
each stored global exactly `float16(1/weight_global_scale)`. This is the check that would
have caught a wrong scale convention. **VERIFIED**

**Model vs checkpoint key diff.** Building the model on the parsed config and diffing
`state_dict()` against the FTW's tensor list: **663 expected, 663 supplied, nothing missing
and nothing extra.** This is what caught part 5 in one shot instead of one serve-crash at a
time; worth repeating for any future checkpoint. **VERIFIED**

**Tests.** `tests/models/test_qwen3_5_moe_config.py`, 13 tests. **7 fail before the fix and
all 13 pass after** -- every one of the five parts has failing-before coverage. The 6 that
pass in both directions are the guards that the modelopt and dense-checkpoint paths did not
regress. `tests/models` + `tests/moe`: **283 passed, 53 skipped, 0 failed**. **VERIFIED**

**End-to-end generation.** Served from the HF checkpoint (`--moe-backend offload`), two
independent requests returned coherent, on-topic, correctly-terminated text -- the check the
previous session flagged as most likely to reveal a subtle scale error, since a wrong
convention shows up as garbage tokens rather than a crash. **VERIFIED**

> *"The sky looks blue because of Rayleigh scattering: sunlight contains all colors, but when
> it hits the tiny molecules in the atmosphere, shorter blue wavelengths scatter much more
> strongly than longer red ones. This scattered blue light is redirected in all directions and
> reaches your eyes from across the sky, giving it its blue color."*
> -- 156 completion tokens, `finish_reason: stop`, reasoning content parsed normally.

---

## 4. How to re-verify

Cheap detector check (~3s, no GPU, no weights):

```bash
~/FreeToken/.venv/bin/python -c '
from freetoken.utils import cached_load_hf_config
from freetoken.models.qwen3_5_moe.config import parse_config
from freetoken.models.qwen3_5_moe.weight import _select_expert_source_spec
p = "~/models/Ornith-1.5-35B-A3B-NVFP4"
c = parse_config(cached_load_hf_config(p))
print(c.expert_quant, c.attn_quant, c.linear_attn_quant, c.dense_quant, c.lm_head_quant)
print(_select_expert_source_spec(p).desc)
'
# nvfp4 nvfp4 none nvfp4 none
# Qwen3.5 NVFP4 experts (compressed-tensors)
```

Key diff -- the check worth running for any new checkpoint, since it finds every
model-vs-checkpoint mismatch at once instead of one `KeyError` per restart:

```python
import dataclasses, json, torch
from freetoken.distributed.info import set_tp_info; set_tp_info(0, 1)
from freetoken.layers.rotary import set_rope_device
from freetoken.utils import cached_load_hf_config
from freetoken.models.qwen3_5_moe.config import parse_config
from freetoken.models.qwen3_5_moe.model import Qwen3_5MoEForCausalLM

FTW = "~/ftw-test/ornith-test-ftw"
cfg = dataclasses.replace(parse_config(cached_load_hf_config(FTW)), moe_backend="offload")
set_rope_device(torch.device("cpu"))
with torch.device("cpu"):
    expected = set(Qwen3_5MoEForCausalLM(cfg).state_dict())
meta = json.load(open(f"{FTW}/freetoken_weight.json"))
supplied = {t["name"] for t in meta["tensors"] if t["kind"] == "weight"}
print(len(expected), len(supplied), expected - supplied, supplied - expected)
```

Conversion and serve (note `~/ftw-test`, **not** `/tmp` -- see §6):

```bash
FREETOKEN_CONVERT_PROGRESS=1 ~/FreeToken/.venv/bin/ft checkpoint \
  --model ~/models/Ornith-1.5-35B-A3B-NVFP4 --out ~/ftw-test/ornith-test-ftw \
  --moe-backend offload

~/FreeToken/.venv/bin/ft serve --model ~/models/Ornith-1.5-35B-A3B-NVFP4 \
  --moe-backend offload --port 30111 --moe-cache-rate 0.08 --cuda-graph-max-bs 1 \
  --max-running-requests 1 --max-prefill-length 2048 --memory-ratio 0.80
```

Tests: `cd ~/FreeToken && .venv/bin/python -m pytest tests/models/test_qwen3_5_moe_config.py -q`.
To confirm they still fail without the fix, `git stash push` the four changed files first
(the test file is untracked, so the stash leaves it in place).

---

## 5. Still open

- **Resident (`--moe-backend fused`) is rejected, not implemented.** A compressed-tensors
  checkpoint stores experts per-expert and un-fused; the resident path wants the pre-fused
  `experts.gate_up_proj`/`down_proj` layout. Part 3 raises a clear error naming the working
  backends. Implementing fuse-on-load is a separate change.
- **TP > 1** is untouched (the family was TP=1 only before this change too).
- Only one checkpoint has been tested. A second compressed-tensors MoE export -- especially
  one whose `ignore` list differs -- would exercise part 5 properly.
- The fix is not upstream. It has not been pushed, and per `AGENTS.md` the user must review
  and own it before any PR.

---

## 6. ft-man still describes upstream behavior -- leave it alone

These places encode the finding and are **still correct**, because they describe the
FreeToken that users actually run:

- `src/compat.rs::unresolvable_experts` and its tests
- `src/ft/preflight.rs::CONVERT_SCRIPT`
- `README.md`, the "A doomed conversion fails in seconds" paragraph

Revisit them only once the fix lands upstream. When that happens, grep the tree for the
distinctive wording rather than trusting this list (see §8).

---

## 7. Environment traps

- **`/tmp` on the server is a tmpfs (32 GB), not disk.** Writing the 21 GiB test FTW into
  `/tmp/ornith-test-ftw` put it in RAM and OOM-killed every serve attempt
  (`memory.events:oom_kill` reached 7, peak 42.9 GB against 40 GiB). The previous handoff
  recommended that path on the strength of "82 GB free", which was the *disk* figure. Convert to `~/ftw-test/` instead. This cost most of an hour; it looked like a bug in the fix.
- 16 GiB VRAM is tight for this model even offloaded. `--moe-cache-rate 0.08
  --cuda-graph-max-bs 1 --max-running-requests 1 --max-prefill-length 2048 --memory-ratio
  0.80` leaves ~2.8 GiB free; the defaults leave 0.68 GiB and OOM on the first prefill.
- A concurrent `pytest tests/moe` run holds ~760 MiB of GPU. Do not run it against a live
  server.
- The server checkout `~/FreeToken` is an editable install, so copying files into
  `python/freetoken/` takes effect immediately. It still points at FlashML upstream; the
  changed files are copied in, not committed there.

---

## 8. Lessons

**Retracting a claim.** (Kept from the original.) A wrong explanation -- that the multimodal
wrapper hid `quantization_config` from the detector -- survived in
`src/ft/preflight.rs`'s doc comment for two commits after being retracted everywhere else,
because the list of places encoding it was incomplete. Grep the tree for the claim's
distinctive words instead of trusting such a list.

**A conversion passing is not the fix working.** Parts 4 and 5 were both invisible to `ft
checkpoint` -- the conversion wrote a perfectly good 20.96 GiB checkpoint that the engine
then refused to load. Anything that changes weight loading has to be taken all the way to a
served model.

**Diff the keys instead of iterating on crashes.** `model.state_dict()` versus the
checkpoint's tensor list found the remaining mismatch immediately, where restarting the
server would have surfaced them one `KeyError` at a time at ~60s each.
