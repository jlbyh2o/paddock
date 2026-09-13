/**
 * A render sweep: every tab, against the fixture, asserting that it mounts without
 * throwing and that the labels §5 names are on the page. This is the web
 * counterpart of the TUI's render sweep.
 */

import { Suspense } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import type { Snapshot } from "../api/types.ts";
import { fixture } from "../mock/fixture.ts";
import { TABS } from "./index.ts";
import { App } from "../App.tsx";

afterEach(() => {
  cleanup();
});

/** Labels that must appear on each tab, from §5's per-tab pane list. */
const EXPECTED: Record<string, string[]> = {
  dashboard: ["Engine", "Throughput", "Cache pools", "Activity", "serving"],
  models: ["Library", "Details", "Qwen3.6-35B-A3B-NVFP4"],
  hub: ["Search", "Files", "unsloth/Qwen3.8-Flash-Next-GGUF"],
  templates: ["Stored", "Preview", "Apply to", "qwen-sharp"],
  serve: ["Knobs", "What it does", "Profiles", "Command"],
  cache: ["Pools", "VRAM budget", "MoE expert slots"],
  jobs: ["Jobs and downloads", "Output"],
  requests: ["Requests", "Detail"],
  logs: ["Engine log"],
};

describe("tab render sweep", () => {
  for (const tab of TABS) {
    it(`renders ${tab.title} against a populated snapshot`, async () => {
      const Body = tab.Component;
      expect(() =>
        render(
          <Suspense fallback={<span>loading</span>}>
            <Body snapshot={fixture} />
          </Suspense>,
        ),
      ).not.toThrow();
      for (const label of EXPECTED[tab.id] ?? []) {
        expect((await screen.findAllByText(new RegExp(escapeRe(label)))).length).toBeGreaterThan(0);
      }
    });
  }
});

describe("tabs that fetch", () => {
  it("Serve loads the knob schema and lists the model group", async () => {
    render(
      <Suspense fallback={<span>loading</span>}>
        <Serve />
      </Suspense>,
    );
    expect(await screen.findByText("Model path or repo id")).toBeTruthy();
  });

  it("Logs shows the engine's lines once the first page arrives", async () => {
    const Body = TABS[8]?.Component;
    if (!Body) throw new Error("no Logs tab");
    render(
      <Suspense fallback={<span>loading</span>}>
        <Body snapshot={fixture} />
      </Suspense>,
    );
    expect(await screen.findByText(/ready to serve/)).toBeTruthy();
  });

  it("Requests shows the ring once the first page arrives", async () => {
    const Body = TABS[7]?.Component;
    if (!Body) throw new Error("no Requests tab");
    render(
      <Suspense fallback={<span>loading</span>}>
        <Body snapshot={fixture} />
      </Suspense>,
    );
    expect((await screen.findAllByText("/v1/chat/completions")).length).toBeGreaterThan(0);
  });
});

describe("serve errors", () => {
  /**
   * §2.12: `serve.errors[].key` is whatever key was in the configuration, and
   * `serve.validate` emits `("<key>", "unknown knob")` for one the schema does not
   * know — a profile written against a newer FreeToken, or a hand-edited file. The
   * Command pane resolves a key to its flag spelling and must fall back to the key
   * itself rather than printing "undefined:".
   */
  it("prints a non-knob error key as itself", async () => {
    const snapshot: Snapshot = {
      ...fixture,
      serve: {
        ...fixture.serve,
        errors: [
          { key: "moe_fanout_beta", message: "unknown knob" },
          { key: "model", message: "a model path or repo id is required" },
        ],
      },
    };
    const def = TABS.find((t) => t.id === "serve");
    if (!def) throw new Error("no Serve tab");
    const Body = def.Component;
    render(
      <Suspense fallback={<span>loading</span>}>
        <Body snapshot={snapshot} />
      </Suspense>,
    );
    // The unknown key prints verbatim; the known one resolves to its flag.
    expect(await screen.findByText("moe_fanout_beta: unknown knob")).toBeTruthy();
    expect(
      await screen.findByText("--model: a model path or repo id is required"),
    ).toBeTruthy();
  });
});

describe("the chrome", () => {
  it("renders every tab button, the status pill and the version", async () => {
    render(<App />);
    for (const tab of TABS) {
      expect(await screen.findByText(tab.title)).toBeTruthy();
    }
    expect((await screen.findAllByText(/serving/)).length).toBeGreaterThan(0);
    expect(await screen.findByText(`v${fixture.version}`)).toBeTruthy();
  });
});

function Serve(): React.ReactNode {
  const def = TABS.find((t) => t.id === "serve");
  if (!def) throw new Error("no Serve tab");
  const Body = def.Component;
  return <Body snapshot={fixture} />;
}

function escapeRe(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
