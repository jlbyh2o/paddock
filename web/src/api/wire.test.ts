/**
 * The wire contract, checked against the daemon's own serialization.
 *
 * `snapshot.populated.json`, `snapshot.empty.json` and `knobs.json` are written by
 * `src/web/tests.rs` with `PADDOCK_DUMP_SNAPSHOTS=1`; they are what `paddock web`
 * actually sends. Two things are asserted here, and both are mechanical because the
 * two halves of the contract were built from the same prose by different hands:
 *
 *  1. Every tab and every overlay renders against both documents without throwing
 *     and without React logging an error.
 *  2. The hand-written fixture in `mock/fixture.ts` has exactly the key set the real
 *     document has, at every level. A key on one side and not the other is a bug on
 *     one side or the other; there is no third possibility.
 */

import { Suspense, createElement } from "react";
import type { ReactElement, ReactNode } from "react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";

import populatedJson from "../mock/snapshot.populated.json" with { type: "json" };
import emptyJson from "../mock/snapshot.empty.json" with { type: "json" };
import knobsJson from "../mock/knobs.json" with { type: "json" };

import type { KnobSchema, Snapshot } from "./types.ts";
import { knobStore } from "./knobs.ts";
import { fixture, mockKnobs } from "../mock/fixture.ts";
import { TABS } from "../tabs/index.ts";
import { ConfirmModal } from "../ui/ConfirmModal.tsx";
import { HelpOverlay } from "../ui/HelpOverlay.tsx";
import { Login } from "../ui/Login.tsx";
import { PlanOverlay } from "../ui/PlanOverlay.tsx";
import { Toasts } from "../ui/Toasts.tsx";

const populated = populatedJson as unknown as Snapshot;
const empty = emptyJson as unknown as Snapshot;
const knobs = knobsJson as unknown as KnobSchema;

beforeAll(() => {
  // Render against the daemon's real schema rather than the mock server's copy.
  knobStore.set(knobs);
});

afterEach(() => {
  cleanup();
});

/**
 * Render one element and fail on anything React complained about. A key warning or a
 * "cannot update during render" is a defect the screenshots would not show.
 */
async function renderClean(element: ReactElement, what: string): Promise<void> {
  const errors: string[] = [];
  const spy = vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
    errors.push(args.map(String).join(" "));
  });
  try {
    await act(async () => {
      render(element);
    });
  } finally {
    spy.mockRestore();
  }
  expect(errors, `${what} logged: ${errors.join(" | ")}`).toEqual([]);
}

/** A tab is lazy, so the sweep waits for the chunk rather than the fallback. */
function suspended(node: ReactNode): ReactElement {
  return createElement(Suspense, { fallback: createElement("span", null, "loading") }, node);
}

describe("every screen renders against the daemon's own documents", () => {
  for (const [label, snapshot] of [
    ["populated", populated],
    ["empty", empty],
  ] as const) {
    for (const tab of TABS) {
      it(`${tab.title} renders against the ${label} snapshot`, async () => {
        await renderClean(
          suspended(createElement(tab.Component, { snapshot })),
          `${tab.title} (${label})`,
        );
      });
    }

    it(`the overlays render against the ${label} snapshot`, async () => {
      await renderClean(createElement(Toasts, { toasts: snapshot.toasts }), `toasts (${label})`);
      await renderClean(createElement(HelpOverlay, { onClose: noop }), `help (${label})`);
      await renderClean(createElement(Login, { onAuthorized: noop }), `login (${label})`);

      const confirm = snapshot.confirm ?? populated.confirm;
      if (confirm) {
        await renderClean(
          createElement(ConfirmModal, { confirm, onAnswer: noop }),
          `confirm (${label})`,
        );
      }
      const plan = snapshot.serve.plan ?? populated.serve.plan;
      if (plan) {
        await renderClean(
          createElement(PlanOverlay, { plan, onApply: noop, onDismiss: noop }),
          `plan (${label})`,
        );
      }
    });
  }
});

// ---------------------------------------------------------------- key sets

/**
 * Every key path in a document.
 *
 * Two shapes need care. Arrays are unioned over their entries, because a list whose
 * first row happens to be the plain case would otherwise hide the fields the
 * interesting rows carry. And a tagged union (§2.3: an object with a `kind`
 * discriminant) contributes its variant as part of the path — `template_status` under
 * `built_in` is a different shape from `template_status` under `overridden`, and
 * comparing them as one object would report a difference that is not one.
 *
 * The discriminant rule also catches a few objects that merely *have* a `kind` field
 * without being a union — `JobEntry.kind` is `"convert"` or `"bench"` — which is
 * harmless: both documents carry both values, and if one stopped carrying one the whole
 * subtree would be reported rather than silently skipped.
 */
function keyPaths(value: unknown, prefix = "", out = new Set<string>()): Set<string> {
  if (Array.isArray(value)) {
    for (const entry of value) keyPaths(entry, `${prefix}[]`, out);
    return out;
  }
  if (!value || typeof value !== "object") return out;

  const record = value as Record<string, unknown>;
  let here = prefix;
  if (typeof record["kind"] === "string") {
    here = `${prefix}<${record["kind"]}>`;
    out.add(here);
  }
  for (const [key, child] of Object.entries(record)) {
    const path = here ? `${here}.${key}` : key;
    out.add(path);
    keyPaths(child, path, out);
  }
  return out;
}

/**
 * Paths whose inner keys are data rather than contract, so a difference in them says
 * nothing about the wire format. Two kinds qualify and no others: a document passed
 * through from the engine verbatim (§2.6 sends `limits`, `last_rebuild` and
 * `sampling` as raw JSON), and a map keyed by a value — `serve.values` is keyed by
 * whichever knobs happen to be set.
 */
const OPAQUE = [
  "telemetry.cache_status.geometry.limits",
  "telemetry.cache_status.last_rebuild",
  "telemetry.stats.model.sampling",
  "hub.results[].gated",
  "hub.info.gated",
  "hardware.bench_profile.dtypes",
  "hardware.bench_profile.dtype_kernels",
  "serve.values",
  "serve.set_counts",
];

/** The `prefix<variant>` markers a document produced. */
function variants(paths: Set<string>): Set<string> {
  const out = new Set<string>();
  for (const path of paths) {
    for (const match of path.matchAll(/[^.[\]]*<[^>]+>/g)) {
      const index = (match.index ?? 0) + match[0].length;
      out.add(path.slice(0, index));
    }
  }
  return out;
}

function comparable(paths: Set<string>, shared: Set<string>): string[] {
  return [...paths]
    .filter((p) => !OPAQUE.some((raw) => p === raw || p.startsWith(`${raw}.`)))
    // A variant only one side produced is not a disagreement about the wire: the
    // encodings themselves are pinned by `the_tagged_enum_encodings_match_the_contract`.
    .filter((p) => [...variants(new Set([p]))].every((v) => shared.has(v)))
    .sort();
}

describe("the hand-written fixture has the real document's shape", () => {
  it("agrees key for key with a populated snapshot", () => {
    const realPaths = keyPaths(populated);
    const mockPaths = keyPaths(fixture);
    const shared = new Set(
      [...variants(realPaths)].filter((v) => variants(mockPaths).has(v)),
    );

    const real = new Set(comparable(realPaths, shared));
    const mock = new Set(comparable(mockPaths, shared));

    const missingFromFixture = [...real].filter((p) => !mock.has(p));
    const extraInFixture = [...mock].filter((p) => !real.has(p));

    expect(
      { missingFromFixture, extraInFixture },
      "the fixture and the daemon must describe the same document",
    ).toEqual({ missingFromFixture: [], extraInFixture: [] });
  });

  it("gives every leaf the type the daemon gives it", () => {
    const real = leafTypes(populated);
    const mock = leafTypes(fixture);
    const disagreements: string[] = [];
    for (const [path, type] of real) {
      const other = mock.get(path);
      // A null on either side says the value was absent, not that the type differs.
      if (other && other !== type) disagreements.push(`${path}: daemon ${type}, fixture ${other}`);
    }
    expect(disagreements).toEqual([]);
  });

  it("the knob schema the fixture carries matches the daemon's", () => {
    const realPaths = keyPaths(knobs);
    const mockPaths = keyPaths(mockKnobs);
    const shared = new Set([...variants(realPaths)].filter((v) => variants(mockPaths).has(v)));
    expect(comparable(realPaths, shared)).toEqual(comparable(mockPaths, shared));
  });
});

/** Every non-null leaf and its JSON type, keyed the way `keyPaths` keys them. */
function leafTypes(value: unknown, prefix = "", out = new Map<string, string>()): Map<string, string> {
  if (Array.isArray(value)) {
    for (const entry of value) leafTypes(entry, `${prefix}[]`, out);
    return out;
  }
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    const here = typeof record["kind"] === "string" ? `${prefix}<${record["kind"]}>` : prefix;
    for (const [key, child] of Object.entries(record)) {
      leafTypes(child, here ? `${here}.${key}` : key, out);
    }
    return out;
  }
  if (value !== null && !OPAQUE.some((raw) => prefix === raw || prefix.startsWith(`${raw}.`))) {
    out.set(prefix, typeof value);
  }
  return out;
}

function noop(): void {
  /* the overlays need a handler, not a behavior, to render */
}
