/**
 * The sampling-override editor in the Models detail pane — §5.3.
 *
 * Three fields, because those are the three keys FreeToken's loader reads out of a
 * checkpoint's `generation_config.json`. They are not `ft serve` flags and never reach an
 * argv: the Serve tab configures the engine, and this configures the model, which is why
 * it lives here rather than beside the knobs.
 *
 * An empty field means *leave the key out*, so the engine falls back to its own default —
 * a distinction a `0` would lose, and the reason these are strings rather than numbers
 * until they are submitted.
 */

import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type { ModelEntry, Sampling } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run } from "../api/store.ts";
import { Field } from "../ui/primitives.tsx";

/** What FreeToken falls back to when the key is absent, shown as the placeholder. */
const FIELDS = [
  { key: "temperature", label: "temperature", fallback: "0.0 (greedy)", step: "any" },
  { key: "top_p", label: "top_p", fallback: "1.0 (off)", step: "any" },
  { key: "top_k", label: "top_k", fallback: "-1 (off)", step: "1" },
] as const;

type FieldKey = (typeof FIELDS)[number]["key"];
type Draft = Record<FieldKey, string>;

const EMPTY: Draft = { temperature: "", top_p: "", top_k: "" };

function toDraft(sampling: Sampling | null): Draft {
  if (!sampling) return EMPTY;
  const show = (v: number | null): string => (v === null ? "" : String(v));
  return {
    temperature: show(sampling.temperature),
    top_p: show(sampling.top_p),
    top_k: show(sampling.top_k),
  };
}

/** `sampling::Sampling::summary` — kept in step with the Rust so both front ends read alike. */
export function summarize(sampling: Sampling): string {
  if (sampling.temperature !== null && sampling.temperature <= 0) return "greedy (temperature 0)";
  const parts: string[] = [];
  if (sampling.temperature !== null) parts.push(`temperature ${sampling.temperature}`);
  if (sampling.top_p !== null) parts.push(`top_p ${sampling.top_p}`);
  if (sampling.top_k !== null) parts.push(`top_k ${sampling.top_k}`);
  return parts.length === 0 ? "unset" : parts.join("  ");
}

/**
 * Parse the three fields the way `SamplingView::parse` and `Sampling::validate` do.
 *
 * The server checks all of this again — it is the only thing that can refuse — but saying
 * so before the round trip is what makes the form usable.
 */
function parse(draft: Draft): { sampling: Sampling } | { error: string } {
  const out: Sampling = { temperature: null, top_p: null, top_k: null };
  for (const field of FIELDS) {
    const raw = draft[field.key].trim();
    if (raw === "") continue;
    const value = Number(raw);
    if (!Number.isFinite(value)) return { error: `${field.label} must be a number, not “${raw}”` };
    if (field.key === "top_k") {
      if (!Number.isInteger(value)) return { error: `top_k must be a whole number, not “${raw}”` };
      out.top_k = value;
    } else if (field.key === "temperature") {
      out.temperature = value;
    } else {
      out.top_p = value;
    }
  }
  if (out.temperature === null && out.top_p === null && out.top_k === null) {
    return { error: "nothing to apply: set at least one of temperature, top_k, top_p" };
  }
  if (out.temperature !== null && (out.temperature < 0 || out.temperature > 2)) {
    return { error: `temperature ${out.temperature} is outside 0.0–2.0` };
  }
  if (out.top_p !== null && (out.top_p <= 0 || out.top_p > 1)) {
    return { error: `top_p ${out.top_p} is outside 0.0–1.0 (and cannot be 0)` };
  }
  if (out.top_k !== null && out.top_k !== -1 && out.top_k < 1) {
    return { error: `top_k ${out.top_k} must be -1 (off) or 1 or more` };
  }
  return { sampling: out };
}

/** `Sampling::warnings` — filters that cannot fire, which the file itself does not show. */
function warnings(sampling: Sampling): string[] {
  const out: string[] = [];
  const filters = sampling.top_k !== null || sampling.top_p !== null;
  if (filters && sampling.temperature === null) {
    out.push(
      "temperature is unset, so it resolves to 0.0 (greedy) and top_k/top_p will have no effect",
    );
  }
  if (filters && sampling.temperature !== null && sampling.temperature <= 0) {
    out.push("temperature is 0 (greedy), so top_k/top_p will have no effect");
  }
  return out;
}

export function SamplingEditor(props: { model: ModelEntry }): ReactNode {
  const model = props.model;
  const [draft, setDraft] = useState<Draft>(() => toDraft(model.sampling_effective));

  // Re-seed when the selection moves, and when a just-applied override comes back on the
  // snapshot — otherwise the form keeps showing what the previous model was serving.
  //
  // Keyed by value, not by identity: the snapshot arrives as fresh JSON several times a
  // second, so depending on the object itself would re-seed on every tick and overwrite
  // whatever was half-typed.
  const seed = JSON.stringify(model.sampling_effective);
  useEffect(() => {
    setDraft(toDraft(model.sampling_effective));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [model.path, seed]);

  const parsed = useMemo(() => parse(draft), [draft]);
  const error = "error" in parsed ? parsed.error : null;
  const sampling = "sampling" in parsed ? parsed.sampling : null;

  if (model.sampling_unsupported) {
    return (
      <>
        <Field label="Sampling">{model.sampling_status.label}</Field>
        <p className="dim">{model.sampling_unsupported}</p>
      </>
    );
  }

  const overridden = model.sampling_status.kind === "overridden";

  return (
    <>
      <Field label="Sampling">
        {model.sampling_effective ? summarize(model.sampling_effective) : "recommends nothing"}
        {overridden ? <span className="dim"> · ft-man override</span> : null}
      </Field>
      <p className="dim">
        Written into the checkpoint’s <span className="mono">generation_config.json</span>, which is
        where FreeToken reads the defaults it applies to a request that sets none of its own. An
        empty field leaves the key out.
      </p>
      <div className="sampling-fields">
        {FIELDS.map((field) => (
          <label key={field.key} className="field">
            <span className="label">{field.label}</span>
            <input
              type="number"
              inputMode="decimal"
              step={field.step}
              value={draft[field.key]}
              placeholder={field.fallback}
              aria-label={`Sampling ${field.label}`}
              onChange={(e) => setDraft((prev) => ({ ...prev, [field.key]: e.target.value }))}
            />
          </label>
        ))}
      </div>
      {error ? <p className="bad">{error}</p> : null}
      {sampling
        ? warnings(sampling).map((warning) => (
            <p key={warning} className="warn">
              {warning}
            </p>
          ))
        : null}
      <div className="inline-form">
        <button
          type="button"
          className="btn primary"
          disabled={sampling === null}
          onClick={() => {
            if (sampling) run(api.applySampling({ path: model.path, ...sampling }));
          }}
        >
          Set sampling
        </button>
        <button
          type="button"
          className="btn"
          disabled={!overridden}
          title={
            overridden
              ? "Restore the checkpoint's own generation_config.json"
              : "No ft-man override is in place"
          }
          onClick={() => run(api.revertSampling({ path: model.path }))}
        >
          Restore checkpoint’s
        </button>
      </div>
    </>
  );
}
