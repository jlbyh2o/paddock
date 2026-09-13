/**
 * Presentation helpers.
 *
 * Every function here mirrors a Rust helper the TUI draws with, so the two front
 * ends print the same string for the same number: `util::bytes`, `util::count`,
 * `util::duration_secs`, `util::rate`, `util::eta` and `plan::tokens`.
 *
 * Nothing here computes anything the engine already computed. These take a number
 * the snapshot supplied and turn it into the text a person reads — and they all
 * accept `null`/`undefined`, returning the em dash the TUI uses for an absent
 * value, so no view ever prints `undefined` or `NaN`.
 */

/** What an absent value renders as, everywhere. */
export const DASH = "—";

/** What an absent *rate* or *estimate* renders as, matching `util::rate`. */
export const DASHES = "--";

type Maybe = number | null | undefined;

function usable(n: Maybe): n is number {
  return typeof n === "number" && Number.isFinite(n);
}

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"] as const;
const SHORT_UNITS = ["B", "K", "M", "G", "T", "P"] as const;

/** `util::bytes` — `12.4 GiB`, `998 MiB`, `512 B`. */
export function bytes(n: Maybe): string {
  if (!usable(n)) return DASH;
  const whole = Math.max(0, Math.trunc(n));
  if (whole < 1024) return `${whole} B`;
  let v = whole;
  let i = 0;
  while (v >= 1024 && i + 1 < BYTE_UNITS.length) {
    v /= 1024;
    i += 1;
  }
  const unit = BYTE_UNITS[i] ?? "B";
  if (v >= 100) return `${v.toFixed(0)} ${unit}`;
  if (v >= 10) return `${v.toFixed(1)} ${unit}`;
  return `${v.toFixed(2)} ${unit}`;
}

/** `util::bytes_short` — `12.4G`, the fixed-width form. */
export function bytesShort(n: Maybe): string {
  if (!usable(n)) return DASH;
  const whole = Math.max(0, Math.trunc(n));
  if (whole < 1024) return `${whole}B`;
  let v = whole;
  let i = 0;
  while (v >= 1024 && i + 1 < SHORT_UNITS.length) {
    v /= 1024;
    i += 1;
  }
  const unit = SHORT_UNITS[i] ?? "B";
  return v >= 10 ? `${v.toFixed(0)}${unit}` : `${v.toFixed(1)}${unit}`;
}

/** A signed byte delta, `+2.00 KiB` / `-2.00 KiB`. */
export function signedBytes(n: Maybe): string {
  if (!usable(n)) return DASH;
  const sign = n < 0 ? "-" : "+";
  return `${sign}${bytes(Math.abs(n))}`;
}

/** `util::count` — `1,234,567`. Locale independent on purpose. */
export function count(n: Maybe): string {
  if (!usable(n)) return DASH;
  const whole = Math.trunc(n);
  const sign = whole < 0 ? "-" : "";
  const digits = Math.abs(whole).toString();
  let out = "";
  for (let i = 0; i < digits.length; i += 1) {
    if (i > 0 && (digits.length - i) % 3 === 0) out += ",";
    out += digits[i];
  }
  return sign + out;
}

/** A signed count, `(+581)` style without the parentheses. */
export function signedCount(n: Maybe): string {
  if (!usable(n)) return DASH;
  return (n < 0 ? "-" : "+") + count(Math.abs(n));
}

/** `util::duration_secs` — `3d 4h`, `1h 10m`, `12m 30s`, `45s`. */
export function duration(secs: Maybe): string {
  if (!usable(secs)) return DASH;
  const total = Math.max(0, Math.trunc(secs));
  const d = Math.floor(total / 86_400);
  const h = Math.floor((total % 86_400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

/** `util::rate` — `310 MiB/s`, or `--` when the rate is not meaningful yet. */
export function rate(bytesPerSec: Maybe): string {
  if (!usable(bytesPerSec) || bytesPerSec <= 0) return DASHES;
  return `${bytes(bytesPerSec)}/s`;
}

/** A download's ETA. The server sends `eta_s`, already null when not meaningful. */
export function eta(secs: Maybe): string {
  if (!usable(secs) || secs < 0) return DASHES;
  return duration(secs);
}

/** `plan::tokens` — `256k`, `233.1k`, `1M`; below 1024 the number itself. */
export function tokens(n: Maybe): string {
  if (!usable(n)) return DASH;
  const v = Math.max(0, Math.trunc(n));
  const MEBI = 1 << 20;
  if (v === 0) return "0";
  if (v >= MEBI && v % MEBI === 0) return `${v / MEBI}M`;
  if (v >= MEBI) return `${(v / MEBI).toFixed(1)}M`;
  if (v >= 1024 && v % 1024 === 0) return `${v / 1024}k`;
  if (v >= 1024) return `${(v / 1024).toFixed(1)}k`;
  return String(v);
}

/** A latency in milliseconds — `8,340 ms`. */
export function ms(n: Maybe): string {
  if (!usable(n)) return DASH;
  return `${count(Math.round(n))} ms`;
}

/** A ratio 0.0–1.0 as whole percent — `87%`. */
export function percent(ratio: Maybe): string {
  if (!usable(ratio)) return DASH;
  return `${Math.round(Math.max(0, Math.min(1, ratio)) * 100)}%`;
}

/** A plain fixed-point number — `48.7`. */
export function fixed(n: Maybe, digits = 1): string {
  if (!usable(n)) return DASH;
  return n.toFixed(digits);
}

/** A number with thousands separators and one decimal — `3,120.5`. */
export function decimal(n: Maybe, digits = 1): string {
  if (!usable(n)) return DASH;
  const parts = Math.abs(n).toFixed(digits).split(".");
  const whole = parts[0] ?? "0";
  const frac = parts[1];
  const sign = n < 0 ? "-" : "";
  const head = `${sign}${count(Number(whole))}`;
  return frac === undefined ? head : `${head}.${frac}`;
}

/** Tokens per second — `48.7 tok/s`, `3,120.5 tok/s`. */
export function tps(n: Maybe): string {
  if (!usable(n)) return DASH;
  return `${decimal(n, 1)} tok/s`;
}

/** GB/s, as the bandwidth profile prints them. */
export function gbs(n: Maybe): string {
  if (!usable(n)) return DASH;
  return `${n.toFixed(1)} GB/s`;
}

/** The `HH:MM:SS` column of a request row, from an ISO-8601 timestamp. */
export function clock(iso: string | null | undefined): string {
  if (!iso) return DASH;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (v: number) => String(v).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** The date part of a Hub `last_modified`, without inventing a timezone. */
export function dateOnly(iso: string | null | undefined): string {
  if (!iso) return DASH;
  const cut = iso.indexOf("T");
  return cut > 0 ? iso.slice(0, cut) : iso;
}

/** A local timestamp for a detail pane — date and time, no seconds fuss. */
export function timestamp(iso: string | null | undefined): string {
  if (!iso) return DASH;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (v: number) => String(v).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** Unix milliseconds as a local timestamp. */
export function timestampMs(msSinceEpoch: Maybe): string {
  if (!usable(msSinceEpoch)) return DASH;
  return timestamp(new Date(msSinceEpoch).toISOString());
}

/** Any text that may be absent. */
export function text(s: string | null | undefined): string {
  if (s === null || s === undefined) return DASH;
  const trimmed = s.trim();
  return trimmed.length === 0 ? DASH : s;
}

/** The first `n` characters of a commit sha, as the Templates pane shows them. */
export function shortSha(sha: string | null | undefined, n = 12): string {
  if (!sha) return DASH;
  return sha.length <= n ? sha : sha.slice(0, n);
}

/** The basename of a path, for a pane title. */
export function basename(path: string | null | undefined): string {
  if (!path) return DASH;
  const parts = path.split("/").filter((p) => p.length > 0);
  return parts.length === 0 ? path : (parts[parts.length - 1] ?? path);
}

/** A tokens-in/tokens-out pair, either half of which may be missing. */
export function tokenPair(a: Maybe, b: Maybe): string {
  return `${count(a)} / ${count(b)}`;
}
