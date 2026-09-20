# The web interface, v2 — redesign plan (draft for review)

This is a plan to make `paddock web` read as a first-class web application rather than
the terminal UI wearing a browser. It is a draft for review, not a commit. The data model,
the API, and the TUI are all left untouched; this changes only what `web/` renders and how
it is styled.

Read alongside [`web-ui.md`](./web-ui.md) (the current design record) and the code:
`web/src/App.tsx` (the shell), `web/src/tabs/*.tsx` (the nine views),
`web/src/ui/primitives.tsx` (the drawing vocabulary), and `web/src/styles.css`.

---

## 1. Where things are now

The web front end is already well built — a dependency-free React 19 + Vite SPA that shares
one `App`, one actions layer, and one `Snapshot` wire format with the TUI. The client does no
engine arithmetic; it formats and lays out what the daemon sends. That is a sound foundation,
and the redesign should preserve it.

What makes it read as *TUI-first* rather than *web-first*:

- **The shell is a terminal status line.** A top bar of tab buttons each carrying a numbered
  key badge (`1`–`9`), a status pill, and a footer line of context-sensitive keyboard hints.
  That is exactly how a ratatui app presents itself; it is not how people browse.
- **Everything is a grid of equal "panes".** The Dashboard is six `.pane` cards in a 3×2 grid,
  and most other tabs are a `.rows` list beside a `.pane` detail. The pane is the TUI's
  box-drawing panel translated to a bordered div. On a wide browser that uniform grid wastes
  horizontal space and gives every subject the same visual weight.
- **The data density is terminal.** Fixed-width label columns (`.field`), one value per row,
  `.facts` rows of dim tokens. Legible, but nothing uses the space a browser gives you.
- **Charts are afterthoughts.** The sparklines are inline-SVG single-series polylines with no
  axis, no hover, no readout — fine for a 28px sliver under a TUI field, thin for a browser.

What is *good* and must survive:

- The shared palette, severity tones, and the "one program, two faces" intent.
- The snapshot-per-frame model, the incremental feeds, the confirm/toast flow, and the
  keyboard handling (`dispatchTabKey`, `useTabKeys`) — all of it is correct and well tested.
- The hand-written CSS and inline-SVG primitives: no component library, no framework. The
  redesign should respect that.

---

## 2. Design goals

1. **Web-first shell.** A navigation structure and chrome that make sense at browser widths,
   not a translated terminal. Keyboard shortcuts stay, but they stop being the primary
   affordance and stop being advertised on every line.
2. **Real charts where the data already lives.** The telemetry series are already streamed
   (decode/prefill throughput, GPU util, VRAM, active requests — 120 samples each). Turn the
   Dashboard into something you can *read*, with axes, hover, and readouts.
3. **Hierarchy over uniformity.** Group related subjects, give the important state room, and
   let the layout breathe at wide widths instead of filling a 1440px screen with six equal boxes.
4. **One program, two faces.** Keep the shared palette and the semantic tones. The web may
   refine elevation, spacing, and typography, but it must still look like the sibling of the TUI.
5. **No framework bloat.** Keep it dependency-light. The one justified dependency is a chart
   library, and only if we choose that route (see §6).

---

## 3. The new shell

### 3.1 Layout

Replace the top tab bar + footer with a **left sidebar + slim header + content area**:

```
┌────────────┬───────────────────────────────────────────────────┐
│ paddock    │  Dashboard                        ● serving …      │  ← slim header: page title + engine status
├────────────┤                                                   │
│ ▤ Dashboard│                                                   │
│ ◻ Models   │                   <content area>                  │
│ ◻ Hub      │                                                   │
│ ◻ Templates│                                                   │
│ ◻ Serve    │                                                   │
│ ◻ Cache    │                                                   │
│ ◻ Jobs     │                                                   │
│ ◻ Requests │                                                   │
│ ◻ Logs     │                                                   │
│            │                                                   │
│ [theme ●]  └───────────────────────────────────────────────────┘
│ [keys ?]   (no footer hint line)
└────────────┘
```

- **Sidebar** (~220px, collapsible to a 64px icon rail). The nine tabs as icon + label + badge.
  Grouped under section headers so nine items read as four groups rather than a wall of buttons:
  *Overview → Dashboard*; *Library → Models, Hub, Templates*; *Serving → Serve, Cache*;
  *Monitoring → Jobs, Requests, Logs*. Badges (active jobs, model count, stored templates) stay.
- **Header** (~48px): the current tab's title on the left, the **engine status pill on the right**
  (the single most important piece of state — it deserves prominence, not a corner token). A
  global search box is a natural web addition here, if one is wanted.
- **Content area**: generous, responsive padding (currently a flat 12px `--gap`); the tab renders
  into it. No footer.

### 3.2 What happens to the TUI-isms

- **Key badges on the tabs go.** In a sidebar nav, a "3" next to "Hub" is noise. This alone
  removes the most obvious terminal tell.
- **The footer hint line goes.** The per-tab `<kbd>e</kbd> start` row is a terminal status line.
  Shortcuts move to the help overlay (§5) and to *inline* hints only where a control genuinely
  needs one (e.g. a commit key beside a form field).
- **The pane stays, but stops being everything.** `.pane` remains the card primitive, but the
  Dashboard stops being "six equal panes" and starts using a deliberate mix: KPI tiles, one hero
  chart, and supporting panels of different sizes.

The shell change is low-risk and self-contained: it wraps the existing tabs, which keep rendering
unchanged until each one is rewritten. That lets us ship the visual win in isolation.

---

## 4. Navigation & keyboard

Keep **all** existing keyboard behavior. `dispatchTabKey` / `useTabKeys` in `web/src/ui/keys.ts`
and the per-tab key handlers are correct and covered by tests; the redesign does not touch them.
Navigation keys (`1`–`9`, `?`, `/`, per-tab keys) keep working through the new shell unchanged.

What changes is *discovery*:

- **The help overlay (`?` / F1) becomes the shortcut surface.** It already exists
  (`web/src/ui/HelpOverlay.tsx`) and already reproduces the TUI's key list tab by tab. It is the
  right home for "all the keys" and is exactly what "don't advertise them all over the place"
  calls for: present on demand, not on every line. No change to its content is required; styling
  is optional.
- **Inline hints replace the footer.** A control that needs a key (a submit field, a toggle) can
  show its commit key once, beside itself. Everything else is silent until `?`.

Net: shortcuts are fully preserved for power users and invisible to everyone else.

---

## 5. The Dashboard — the charts centerpiece

The Dashboard is where a browser earns its keep. It currently shows six panes: **Engine,
Throughput, Cache pools, GPU, Host, Activity**. Keep those six subjects; reorganize them into a
hierarchy instead of a uniform grid.

Proposed structure:

1. **A row of KPI tiles across the top** — compact, scannable, one number each: Status, Model,
   Context fit (`64k of 256k`), Decode TPS, Active requests, Completed/s, Uptime. These are the
   numbers you check on entry; put them where the eye goes first.
2. **A hero throughput chart** (full content width) — decode + prefill as two series on one
   time-scaled axis, with a crosshair and a live readout. This is the one chart that should be
   big. It consumes `series.decode_tps` / `series.prefill_tps`.
3. **Second row, three panels:**
   - **GPU/VRAM live chart** — GPU util% and VRAM MiB overlaid, same time axis as the hero.
     Consumes `series.gpu_util` / `series.vram`.
   - **Cache VRAM breakdown** — a single *broken bar* (weights + KV + MoE + GDN + SWA) instead of
     four separate meters. This is a genuinely better representation of "how is my VRAM split"
     and is the kind of thing a terminal cannot do. Consumes `telemetry.pool_bytes` against
     `stats.vram_bytes` / `cache.budget_bytes`.
   - **Host gauges** — CPU and RAM as small gauges/sparklines, plus load and free-for-experts.
4. **Third row** — the Engine fields (status, endpoint, pid, uptime, sampling, checkout/origin
   summary) tidied into a panel; the Activity panel with latency, prefix reuse, and the request
   history sparkline; and the truncated-context warning kept where it is now.

The Engine pane's checkout/origin-summary block (the "what changed?" summary) is prose and stays
as prose — it is the one place in the Dashboard you *read* rather than *scan*.

---

## 6. Charts approach

**Chosen: add `uPlot`** (see §11). `uPlot` is a small (~13 KB gz), dependency-free, canvas-based
time-series chart. It is built for exactly this: telemetry at 1–10 Hz, multiple series, crosshair
and a value readout, smooth at width. It carries the hero throughput chart, the GPU/VRAM chart, and
any other live time-series. The payoff is real — axes, hover, and a live readout turn "a line"
into something you can actually read.

The small inline meters and the compact sparklines under a field stay as the current inline-SVG
primitives — they are simple enough that a chart library would be overkill.

For static comparisons, a lightweight horizontal **bar chart** suits the Jobs tab's bandwidth
profile (GB/s per format: CPU vs PCIe), which is currently a table.

---

## 7. Per-tab plan

Most tabs are already usable; the work is polish and, where it earns its place, a chart. The
Dashboard (§5) is the priority.

- **Dashboard** (§5): KPIs, hero throughput chart, GPU/VRAM chart, cache VRAM broken bar, host
  gauges. The biggest single win.
- **Models**: keep the list + detail split (it is right for density). Make the detail a richer
  card, and add a per-model context-fit indicator (the "64k of 256k" warning) as a small visual
  rather than a dim line. Keep the `.fmt` tags.
- **Hub**: promote the compatibility verdict to a prominent panel. Visualize the servable-context
  figure (e.g. a bar showing `44k of 256k advertised`) — the single most decision-relevant number
  on the page. Keep search/quant/files structure.
- **Templates**: largely fine as-is. Minor polish; the apply-to-model picker is already web-native.
- **Serve**: keep the grouped knob list, the "what it does" pane, and the command preview. Consider
  turning "what it does" into a per-knob expandable row so the detail pane disappears. The knob
  inputs are small; give them more room.
- **Cache**: the per-pool sliders are already the right web control. Reuse the cache VRAM broken
  bar from the Dashboard here too. Keep.
- **Jobs**: add the bandwidth-profile bar chart (§6). Keep the list + output split.
- **Requests**: add a small latency / status-trend chart at the top of the table. Keep the table
  and detail pane.
- **Logs**: leave as a monospace log view. A live log *is* terminal-shaped; that is appropriate.

---

## 8. Theme & styling

- **Keep the shared palette** (`--fg`, `--accent`, `--good/--warn/--bad`, the TUI-derived light and
  dark). This is the "two faces" promise.
- **Add a scale, not a new look.** Introduce CSS custom-property scales for spacing and type so the
  new shell has consistent rhythm, and refine elevation (shadow layers for the sidebar/header/cards),
  radius, and a small type scale. Hand-written CSS stays — no Tailwind/MUI/etc.
- **Motion, sparingly.** A subtle transition on the collapsible sidebar and on hover states; nothing
  that competes with the live numbers.

---

## 9. Technical constraints & data model

- **No backend changes needed.** All chart data is already streamed in the `Snapshot`: `series.*`
  (120 samples each of decode_tps, prefill_tps, gpu_util, vram, active) and the live
  `telemetry`/`hardware`/`cache` fields. Charts consume these directly; the client still does no
  arithmetic. If a richer history is wanted later it is a small `web/` side change (keep more
  samples), not a server change.
- **Cadence is fine for live charts.** Engine telemetry polls at `server.poll_ms` (default 1000 ms,
  floor 200 ms), hardware at 1000 ms, and the wire coalesces to ≤10 frames/s. A canvas chart at
  that rate is effortless.
- **The snapshot model is unchanged.** Every frame replaces the last; charts read the current
  `series` arrays and re-render on each frame.
- **Embedding is unchanged.** `web/dist` is still built explicitly and embedded via `rust-embed`;
  `build.rs` still drops a placeholder when it is absent. `cargo build` needs no Node.

---

## 10. Testing

The suite is behavior-focused (`web/src/tabs/behavior.test.tsx` names the defect each case
prevents) and asserts on selectors — `.loglist`, `.table-wrap`, `tbody tr`, input labels, button
names — plus the `<App />` shell in one case.

- **Behavior tests mostly survive.** They test *what a tab does* (following a capped window, the
  detail pane showing the newest request, filter trimming, the apply-target reconciliation, the
  field-bound error, the confirm-modal focus), not *how it is laid out*. Those invariants stay
  true through the redesign and should keep their assertions.
- **Shell and selector tests need edits.** The `App` test and any test that keys off the tab bar,
  the key badges, or the footer hint line will need their selectors updated to the new shell.
  New chart components get their own small render/scale tests.
- **The mock fixture stays** as the data source for `npm run dev` and the tests; charts render from
  it in both.

Rule of thumb for the rewrite: keep each phase's `npm run check` green before moving on.

---

## 11. Phased rollout

Each phase is shippable and keeps the suite green.

| Phase | Scope | Why first |
|------|-------|-----------|
| **A — Shell** | New sidebar + header, migrate chrome (theme, connection, toasts, modals, help), establish spacing/type/elevation tokens. Old tabs keep rendering until rewritten. | Immediate visual win, fully isolated, unblocks everything else. |
| **B — Charts foundation** | `uPlot` (or the SVG `Chart` primitive) + a wrapper, axes/tooltip/hookup to `series`. | Lets Phase C render real charts. |
| **C — Dashboard** | KPIs, hero throughput chart, GPU/VRAM chart, cache VRAM broken bar, host gauges. | Highest-value surface; the reason for the whole effort. |
| **D — Other tabs** | Models, Hub, Serve, Cache, Jobs, Requests, Logs polish + charts where they earn their place. | Turns one good screen into a coherent app. |
| **E — Keyboard surface** | Help-overlay styling, footer removal, de-emphasize keys. | Realizes the "don't advertise them" goal. |
| **F — Theme refinement** | Elevation, radius, typography, motion polish. | Finishing touch; optional. |

A conservative path is **A → B → C** and then stop to look at it before committing to D–F.

---
## 12. Decisions

All five were decided.

1. **Navigation — left sidebar, grouped** into Overview / Library / Serving / Monitoring.
2. **Charts — add `uPlot`.**
3. **Scope — dashboard-first (Phases A→C), review, then decide on D–F.**
4. **Palette — shared base, small web-only touches.** Keep the TUI palette, but let the web add a
   touch of its own: a slightly different accent, a richer dark theme, more layered elevation.
   Still clearly a sibling, with a bit more personality.
5. **Icons — small icon set.** Icon + label per tab, drawn as a consistent set of inline-SVG
   glyphs, so the collapsible icon-rail mode earns its place.

</content>
</invoke>

<tool_call>
<function=bash>
<parameter=command>
cd /home/jeremy/Code/personal_projects/paddock && wc -l docs/web-ui-redesign.md && echo "---" && ls docs/