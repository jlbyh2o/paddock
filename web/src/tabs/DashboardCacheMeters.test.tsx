/**
 * The Cache pools card shows one utilization meter per pool (KV, MoE, GDN state,
 * SWA), read from the pool sizes the engine reports while serving. This pins that
 * the meters render — and move — rather than the old dead byte bar that stayed 0
 * until the engine published per-unit costs.
 */

import { Suspense } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import type { Snapshot } from "../api/types.ts";
import { Dashboard } from "./Dashboard.tsx";
import { fixture } from "../mock/fixture.ts";

function mount(snapshot: Snapshot) {
  render(
    <Suspense fallback={<span>loading</span>}>
      <Dashboard snapshot={snapshot} />
    </Suspense>,
  );
}

afterEach(() => {
  cleanup();
});

/** The token/expert/slot figures only the pool meters print — the CacheBar shows bytes. */
const LIVE = ["16,384 / 65,536 tok", "2,403 / 6,144 experts", "12 / 64 slots", "8,192 / 32,768 tok"];

describe("dashboard cache pools meters", () => {
  it("renders one live meter per pool with the engine's figures", () => {
    mount(fixture);
    for (const label of ["KV", "MoE", "GDN state", "SWA"]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    for (const figure of LIVE) {
      expect(screen.getByText(figure)).toBeTruthy();
    }
  });

  it("falls back to an idle meter for each pool when the engine has published nothing", () => {
    const empty: Snapshot = {
      ...fixture,
      telemetry: {
        ...fixture.telemetry,
        stats: null,
        cache_status: null,
        total_experts: null,
        kv_used_tokens: null,
        kv_total_tokens: null,
        kv_ratio: null,
        swa_used_tokens: null,
        swa_total_tokens: null,
        swa_ratio: null,
        mamba_ratio: null,
      },
    };
    mount(empty);
    // The pool labels still anchor each row; the live figures are gone.
    for (const label of ["KV", "MoE", "GDN state", "SWA"]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    for (const figure of LIVE) {
      expect(screen.queryByText(figure)).toBeNull();
    }
  });
});
