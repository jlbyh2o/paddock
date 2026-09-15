#!/usr/bin/env python3
"""Reference implementation of docs/kv-geometry.md.

Prices a checkpoint's KV cache from its config.json alone, the way compat.rs should.
Kept so the Rust port has something to check against, and so the question is answerable
before that lands. Reads only config.json -- one small request, no weights.

    scripts/kv-geometry.py Qwen/Qwen3.6-35B-A3B
    scripts/kv-geometry.py IFM/K2-Horizon-MoVA-36B-A4B --ctx 262144 --vram 16 --weights 5
    scripts/kv-geometry.py ./config.json

--vram and --weights are GiB; given both, it reports the largest context this card could
actually serve. KV is bf16 because FreeToken has no KV dtype flag; --fp8 previews the
halving the in-flight work would buy.
"""
import argparse, json, sys, urllib.request

GROWING = {"full_attention", "deepseek_sparse_attention"}
WINDOWED = {"sliding_attention"}
FLAT = {"linear_attention", "kda", "mamba", "linear"}


def load(src):
    if src.endswith(".json"):
        with open(src) as f:
            return json.load(f)
    url = f"https://huggingface.co/{src}/raw/main/config.json"
    req = urllib.request.Request(url, headers={"User-Agent": "paddock-kv-geometry"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def geometry(cfg, elem=2):
    """-> (growing, windowed, flat, row_bytes, window, shape) from a config.json."""
    t = cfg.get("text_config", cfg)
    layers = t["num_hidden_layers"]
    window = t.get("sliding_window") if t.get("use_sliding_window", True) else None

    types = t.get("layer_types")
    if types:
        growing = sum(1 for x in types if x in GROWING)
        windowed = sum(1 for x in types if x in WINDOWED)
        flat = sum(1 for x in types if x in FLAT)
        unknown = len(types) - growing - windowed - flat
        if unknown:  # an unrecognized name is priced as the expensive case, not ignored
            growing += unknown
    elif window:
        growing, windowed, flat = 0, layers, 0
    else:
        growing, windowed, flat = layers, 0, 0

    if t.get("kv_lora_rank"):  # MLA: one compressed latent per token, not per head
        row = (t["kv_lora_rank"] + t.get("qk_rope_head_dim", 0)) * elem
        shape = f"MLA latent {t['kv_lora_rank'] + t.get('qk_rope_head_dim', 0)}"
    else:
        kvh = t.get("num_key_value_heads") or t["num_attention_heads"]
        hd = t.get("head_dim") or t["hidden_size"] // t["num_attention_heads"]
        row = 2 * kvh * hd * elem
        shape = f"{kvh} kv heads x {hd}"
    return growing, windowed, flat, row, window, shape


def main():
    p = argparse.ArgumentParser()
    p.add_argument("model", help="HF repo id, or a path to a config.json")
    p.add_argument("--ctx", type=int, default=None, help="tokens (default: the model's ceiling)")
    p.add_argument("--vram", type=float, default=0.0, help="GiB")
    p.add_argument("--weights", type=float, default=0.0, help="GiB resident after offload")
    p.add_argument("--reserve", type=float, default=2.0, help="GiB left for experts/state/slack")
    p.add_argument("--fp8", action="store_true", help="preview fp8 KV")
    a = p.parse_args()

    cfg = load(a.model)
    t = cfg.get("text_config", cfg)
    ceiling = t.get("max_position_embeddings", 0)
    ctx = a.ctx or ceiling or 262144
    grow, win_n, flat, row, window, shape = geometry(cfg, elem=1 if a.fp8 else 2)

    per_token = grow * row
    capped = win_n * row * min(ctx, window or ctx)
    total = per_token * ctx + capped
    gib = 1 << 30

    print(f"{a.model}")
    print(f"  {t['num_hidden_layers']} layers: {grow} growing, {win_n} windowed({window}), {flat} flat")
    print(f"  per growing layer, per token: {shape} = {row/1024:.1f} KiB {'fp8' if a.fp8 else 'bf16'}")
    print(f"  KV per token: {per_token/1024:.1f} KiB")
    print(f"  at {ctx:,} tokens: {total/gib:.2f} GiB"
          + (f" (incl. {capped/gib:.2f} GiB capped window)" if capped else ""))
    if ceiling:
        print(f"  advertised ceiling: {ceiling:,} tokens")

    if a.vram and per_token:
        budget = (a.vram - a.weights - a.reserve) * gib - capped
        if budget <= 0:
            print(f"\n  BLOCKER: {a.vram:g} GiB VRAM leaves nothing for KV after "
                  f"{a.weights:g} GiB of weights and {a.reserve:g} GiB reserved")
            return
        servable = int(budget / per_token)
        pct = 100 * servable / ceiling if ceiling else 0
        verdict = "OK" if pct >= 100 else ("CAUTION" if pct >= 25 else "BLOCKER")
        if ceiling and servable >= ceiling:
            # The ceiling is the model's, not the card's; never advertise past it.
            headroom = (budget - ceiling * per_token) / gib
            print(f"\n  {verdict}: reaches its full {ceiling/1024:,.0f}k ceiling on this "
                  f"card, with {headroom:.2f} GiB to spare")
        else:
            print(f"\n  {verdict}: serves about {servable/1024:,.0f}k of its advertised "
                  f"{ceiling/1024:,.0f}k on this card"
                  + (f" ({pct:.0f}%)" if ceiling else ""))
        print(f"    budget: {a.vram:g} - {a.weights:g} weights - {a.reserve:g} reserved "
              f"= {budget/gib:.2f} GiB for KV")


if __name__ == "__main__":
    sys.exit(main())
