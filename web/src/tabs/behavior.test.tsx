/**
 * Behavior the render sweep cannot see: what follows, what stays selected, what keeps
 * the focus, and what happens to a target that vanished under the reader.
 *
 * Each test here stands for one defect that shipped, so each one names it.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { LogLine, RequestRecord, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { localToastStore, logFeed, requestFeed, snapshotStore } from "../api/store.ts";
import { fixture } from "../mock/fixture.ts";
import { dispatchTabKey } from "../ui/keys.ts";
import { ConfirmModal } from "../ui/ConfirmModal.tsx";
import { Logs } from "./Logs.tsx";
import { Models } from "./Models.tsx";
import { Requests } from "./Requests.tsx";
import { Serve } from "./Serve.tsx";
import { Templates } from "./Templates.tsx";

afterEach(() => {
  cleanup();
  logFeed.reset();
  requestFeed.reset();
  snapshotStore.set(null);
  vi.restoreAllMocks();
});

/** jsdom lays nothing out, so a scrollable element needs a height to scroll to. */
function withScrollHeight(px: number): () => void {
  const original = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
    configurable: true,
    get: () => px,
  });
  return () => {
    if (original) Object.defineProperty(HTMLElement.prototype, "scrollHeight", original);
  };
}

function logLine(seq: number): LogLine {
  return { seq, text: `line ${seq}`, err: false, severity: "normal" };
}

function record(seq: number, model: string): RequestRecord {
  return {
    seq,
    ts: "2026-09-13T09:21:44.118000Z",
    method: "POST",
    path: `/v1/chat/completions?n=${seq}`,
    status: 200,
    model,
    duration_ms: 1000,
    ttft_ms: 100,
    prompt_tokens: 10,
    completion_tokens: 20,
    stream: true,
    error: null,
    decode_tps: 20,
  };
}

/** A snapshot whose log counters agree with a buffer holding `seqs`. */
function withLogs(seqs: number[]): Snapshot {
  const last = seqs[seqs.length - 1] ?? 0;
  return {
    ...fixture,
    logs: { ...fixture.logs, count: seqs.length, first_seq: seqs[0] ?? 0, last_seq: last, dropped: 0 },
  };
}

function withRequests(seqs: number[]): Snapshot {
  const last = seqs[seqs.length - 1] ?? 0;
  return {
    ...fixture,
    requests: {
      ...fixture.requests,
      count: seqs.length,
      first_seq: seqs[0] ?? 0,
      last_seq: last,
      dropped: 0,
    },
  };
}

/**
 * B1. Both views scrolled to the tail on the *length* of what they render, and both
 * render a capped window — 800 lines, 512 requests — so once the cap was reached the
 * length stopped changing and following silently stopped.
 */
describe("following a capped window", () => {
  it("Logs follows a new line after the window is full", () => {
    const restore = withScrollHeight(4000);
    try {
      const seqs = Array.from({ length: 900 }, (_, i) => i + 1);
      logFeed.store.set({
        items: seqs.map(logLine),
        heldSeq: 900,
        dropped: 0,
        gapDropped: 0,
      });

      const view = render(<Logs snapshot={withLogs(seqs)} />);
      const body = view.container.querySelector(".loglist");
      if (!(body instanceof HTMLElement)) throw new Error("no log body");
      expect(body.scrollTop).toBe(4000);

      body.scrollTop = 0;
      const grown = [...seqs, 901];
      act(() => {
        logFeed.store.set({ items: grown.map(logLine), heldSeq: 901, dropped: 0, gapDropped: 0 });
      });
      view.rerender(<Logs snapshot={withLogs(grown)} />);
      expect(body.scrollTop).toBe(4000);
    } finally {
      restore();
    }
  });

  it("Requests follows a new entry after the ring is full", () => {
    const restore = withScrollHeight(4000);
    try {
      const seqs = Array.from({ length: 512 }, (_, i) => i + 1);
      const items = seqs.map((seq) => record(seq, `model-${seq}`));
      requestFeed.store.set({ items, heldSeq: 512, dropped: 0, gapDropped: 0 });

      const view = render(<Requests snapshot={withRequests(seqs)} />);
      const body = view.container.querySelector(".table-wrap");
      if (!(body instanceof HTMLElement)) throw new Error("no request body");
      expect(body.scrollTop).toBe(4000);

      body.scrollTop = 0;
      // The ring evicts from the front as it appends: same length, new newest entry.
      const rolled = [...items.slice(1), record(513, "model-513")];
      const rolledSeqs = rolled.map((r) => r.seq);
      act(() => {
        requestFeed.store.set({ items: rolled, heldSeq: 513, dropped: 1, gapDropped: 0 });
      });
      view.rerender(<Requests snapshot={withRequests(rolledSeqs)} />);
      expect(body.scrollTop).toBe(4000);
    } finally {
      restore();
    }
  });
});

/**
 * B4. With no explicit selection the detail pane showed `items[0]`, the *oldest*
 * entry in the ring, while the table scrolled to the newest — so the pane described a
 * request that had scrolled off the top.
 */
describe("the Requests detail pane", () => {
  const seqs = [1, 2, 3];
  const items = [record(1, "oldest-model"), record(2, "middle-model"), record(3, "newest-model")];

  function mount() {
    requestFeed.store.set({ items, heldSeq: 3, dropped: 0, gapDropped: 0 });
    return render(<Requests snapshot={withRequests(seqs)} />);
  }

  it("shows the newest entry while following", () => {
    mount();
    expect(screen.getByText("newest-model")).toBeTruthy();
    expect(screen.queryByText("oldest-model")).toBeNull();
  });

  it("pins the row that was clicked, and f resumes following", () => {
    const view = mount();
    const rows = view.container.querySelectorAll("tbody tr");
    const first = rows[0];
    if (!(first instanceof HTMLElement)) throw new Error("no rows");

    fireEvent.click(first);
    expect(screen.getByText("oldest-model")).toBeTruthy();
    expect(screen.queryByText("newest-model")).toBeNull();

    act(() => {
      dispatchTabKey(new KeyboardEvent("keydown", { key: "f" }));
    });
    expect(screen.getByText("newest-model")).toBeTruthy();
  });
});

/**
 * B3. The filter was trimmed for the "is anything being filtered" test but *not* for
 * the match, so a trailing space turned a full library into "Nothing matches".
 */
describe("the Models filter", () => {
  it("ignores surrounding whitespace", () => {
    render(<Models snapshot={fixture} />);
    const box = screen.getByLabelText("Filter the library");

    fireEvent.change(box, { target: { value: " qwen " } });
    expect(screen.getAllByText("Qwen3.6-35B-A3B-NVFP4").length).toBeGreaterThan(0);
    expect(screen.queryByText(/Nothing matches/)).toBeNull();
    expect(screen.queryByText("Llama-4.2-11B-Instruct")).toBeNull();

    fireEvent.change(box, { target: { value: "   " } });
    expect(screen.getByText("Llama-4.2-11B-Instruct")).toBeTruthy();
  });
});

/**
 * B2. The apply target was captured once, at mount, and never reconciled — so after a
 * rescan or a delete the tab kept aiming `POST /api/templates/apply` at a checkpoint
 * that was no longer in the library.
 */
describe("the Templates apply target", () => {
  const llama = fixture.models.items[2];

  it("falls back to a checkpoint that still exists, and disables the actions when none does", () => {
    if (!llama) throw new Error("the fixture lost its third model");
    const view = render(<Templates snapshot={fixture} />);

    const picker = screen.getByLabelText("Model");
    fireEvent.change(picker, { target: { value: llama.path } });
    expect(screen.getByText(llama.template_status.label)).toBeTruthy();

    // A rescan removes it.
    const without: Snapshot = {
      ...fixture,
      models: { ...fixture.models, items: fixture.models.items.filter((m) => m !== llama) },
    };
    view.rerender(<Templates snapshot={without} />);
    const first = without.models.items[0];
    if (!first) throw new Error("no models left");
    expect(screen.getByText(first.template_status.label)).toBeTruthy();
    const restore = screen.getByRole("button", { name: /Restore built-in/ });
    expect((restore as HTMLButtonElement).disabled).toBe(false);

    // And with nothing in the library there is no target at all.
    const empty: Snapshot = { ...fixture, models: { ...fixture.models, items: [] } };
    view.rerender(<Templates snapshot={empty} />);
    for (const name of [/Apply/, /Restore built-in/, /Verify/]) {
      const button = screen.getByRole("button", { name });
      expect((button as HTMLButtonElement).disabled).toBe(true);
    }
  });
});

/**
 * A1. §1.2: a refusal the daemon marked `toasted: false` belongs to the field that
 * produced it. The client used to guess that from the status code, which made every
 * 409 a field error somewhere and a floating toast somewhere else.
 */
describe("a rejected knob value", () => {
  it("lands under its own field and raises no toast", async () => {
    render(<Serve snapshot={fixture} />);
    fireEvent.click(await screen.findByRole("button", { name: /Server/ }));

    const port = await screen.findByLabelText("Bind port");
    fireEvent.change(port, { target: { value: "99999" } });
    await act(async () => {
      fireEvent.blur(port);
    });

    expect(screen.getByText("--port: must be at most 65535")).toBeTruthy();
    expect(localToastStore.get()).toEqual([]);
  });
});

/**
 * B5. The focus effect was keyed on the `confirm` object, which is rebuilt by every
 * snapshot — ten times a second — so focus was dragged back to Cancel while the reader
 * was trying to reach the other button.
 */
describe("the confirmation modal's focus", () => {
  const confirm = fixture.confirm;

  it("stays where the reader put it across identical snapshots", () => {
    if (!confirm) throw new Error("the fixture lost its confirmation");
    const view = render(<ConfirmModal confirm={confirm} onAnswer={() => {}} />);
    const accept = screen.getByRole("button", { name: /Confirm/ });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: /Cancel/ }));

    (accept as HTMLButtonElement).focus();
    // The next frame: a new object, the same words.
    view.rerender(
      <ConfirmModal confirm={{ ...confirm, body: [...confirm.body] }} onAnswer={() => {}} />,
    );
    expect(document.activeElement).toBe(accept);

    // A different confirmation is a different question, and starts on the safe option.
    view.rerender(
      <ConfirmModal confirm={{ ...confirm, title: "Stop the engine" }} onAnswer={() => {}} />,
    );
    expect(document.activeElement).toBe(screen.getByRole("button", { name: /Cancel/ }));
  });
});

/**
 * B6. Enter accepted whatever was pending, whichever button had the focus — so tabbing
 * to Cancel and pressing Enter deleted the checkpoint.
 */
describe("Enter while a confirmation is up", () => {
  it("activates the focused button rather than accepting", async () => {
    const answered = vi.spyOn(api, "confirm").mockResolvedValue({ status: "ok" });
    const { App } = await import("../App.tsx");
    const mock = await import("../mock/server.ts");
    render(<App />);
    await act(async () => {});

    await act(async () => {
      mock.handle("POST", "/api/models/delete", { path: "/workspace/ftw/whatever" });
    });

    const cancel = await screen.findByRole("button", { name: /Cancel/ });
    const accept = screen.getByRole("button", { name: /Confirm/ });
    expect(document.activeElement).toBe(cancel);

    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    expect(answered).toHaveBeenLastCalledWith({ accept: false });

    (accept as HTMLButtonElement).focus();
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    expect(answered).toHaveBeenLastCalledWith({ accept: true });

    // y and n stay unambiguous whatever has the focus.
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", bubbles: true }));
    });
    expect(answered).toHaveBeenLastCalledWith({ accept: false });

    await act(async () => {
      mock.handle("POST", "/api/confirm", { accept: false });
    });
  });
});
