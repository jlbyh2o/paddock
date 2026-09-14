/**
 * Models — §5.3.
 *
 * The list arrives unfiltered (the snapshot says so explicitly) and filtering is a
 * browser concern, matching the TUI's rule: name, path or arch contains the needle,
 * case-insensitively. Every action names the model by `path`, never by row.
 */

import { useCallback, useMemo } from "react";
import type { ReactNode } from "react";
import type { ModelEntry, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Bullets, Empty, Field, Pane } from "../ui/primitives.tsx";
import { SearchField, useFilterField } from "../ui/SearchField.tsx";
import { SamplingEditor } from "./SamplingEditor.tsx";
import { DASH, bytes, count, text, timestampMs, tokens } from "../format.ts";

/** The TUI's rule, against a needle that was trimmed and lowercased by the caller. */
function matches(model: ModelEntry, needle: string): boolean {
  return (
    model.name.toLowerCase().includes(needle) ||
    model.path.toLowerCase().includes(needle) ||
    (model.arch ?? "").toLowerCase().includes(needle)
  );
}

export function Models(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const filter = useFilterField();
  // Trimmed once: a filter of two spaces is not a filter, and the same string must
  // decide both "is anything being filtered" and what the rows are matched against.
  const needle = filter.needle.toLowerCase();

  const items = useMemo(
    () => (needle === "" ? s.models.items : s.models.items.filter((m) => matches(m, needle))),
    [s.models.items, needle],
  );

  const selection = useSelection(items, useCallback((m: ModelEntry) => m.path, []));
  const model = selection.item;

  const useModel = useCallback((path: string, andServe: boolean) => {
    run(api.useModel({ path, and_serve: andServe }));
  }, []);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (filter.handleKey(event)) return true;
        if (event.key === "Escape") return false;
        if (selection.handleKey(event)) return true;
        switch (event.key) {
          case "Enter":
            if (model) useModel(model.path, false);
            return true;
          case "s":
            if (model) useModel(model.path, true);
            return true;
          case "c":
            if (model) run(api.convertModel({ path: model.path }));
            return true;
          case "D":
            if (model) run(api.deleteModel({ path: model.path }));
            return true;
          case "r":
            run(api.rescanModels());
            return true;
          default:
            return false;
        }
      },
      [filter, selection, model, useModel],
    ),
  );

  const total = s.models.items.length;
  const title = s.models.scanning ? "Library (scanning…)" : "Library";
  const note = needle === "" ? `${count(total)}` : `${count(items.length)} of ${count(total)}`;

  return (
    <div className="panes wide">
      <Pane
        title={title}
        note={note}
        bodyClassName="flush"
        actions={
          <>
            <SearchField
              field={filter}
              placeholder="filter  (/)"
              label="Filter the library"
              style={{ flex: "1 1 200px" }}
            />
            <button type="button" className="btn" onClick={() => run(api.rescanModels())}>
              Rescan <span className="dim">(r)</span>
            </button>
          </>
        }
      >
        {total === 0 ? (
          <Empty>
            <p>No checkpoints found. These roots were scanned:</p>
            <ul>
              {s.models.roots.map((root) => (
                <li key={root.path} className="mono">
                  {root.path}
                  {root.exists ? "" : " (does not exist)"}
                </li>
              ))}
            </ul>
            <p>
              Add a root in <span className="mono">{s.models.config_path}</span>.
            </p>
          </Empty>
        ) : items.length === 0 ? (
          <Empty>
            <p>Nothing matches “{filter.needle}”.</p>
          </Empty>
        ) : (
          <ul className="rows scroll h-560">
            {items.map((entry) => (
              <li
                key={entry.path}
                className={`row ${entry.path === selection.id ? "selected" : ""}`}
                onClick={() => selection.select(entry.path)}
                onDoubleClick={() => useModel(entry.path, false)}
              >
                <div className="row-main">
                  <span className={`fmt ${entry.format === "partial_ftw" ? "part" : entry.format}`}>
                    {entry.format_label}
                  </span>
                  <span className="grow">{entry.name}</span>
                  {entry.converted_to ? <span className="good" title="an FTW build exists">→</span> : null}
                  <span className="dim nowrap">{bytes(entry.size_bytes)}</span>
                </div>
                <div className="row-sub">{entry.summary}</div>
              </li>
            ))}
          </ul>
        )}
      </Pane>

      <Pane
        title="Details"
        actions={
          model ? (
            <>
              <button type="button" className="btn" onClick={() => useModel(model.path, false)}>
                Use <span className="dim">(Enter)</span>
              </button>
              <button
                type="button"
                className="btn primary"
                onClick={() => useModel(model.path, true)}
                disabled={model.is_partial}
              >
                Serve now <span className="dim">(s)</span>
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => run(api.convertModel({ path: model.path }))}
                disabled={!model.convertible}
              >
                Convert <span className="dim">(c)</span>
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => run(api.deleteModel({ path: model.path }))}
              >
                Delete <span className="dim">(D)</span>
              </button>
            </>
          ) : null
        }
      >
        {!model ? (
          <Empty>
            <p>Select a checkpoint to see what paddock knows about it.</p>
          </Empty>
        ) : (
          <>
            <Field label="Name">{model.name}</Field>
            <Field label="Path" mono>
              {model.path}
            </Field>
            <Field label="Format">{model.format_description}</Field>
            <Field label="Size">{bytes(model.size_bytes)}</Field>
            <Field label="Modified">{timestampMs(model.modified_ms)}</Field>
            <Field label="Architecture">{text(model.arch)}</Field>
            <Field label="Model type">{text(model.model_type)}</Field>
            <Field label="Quant">{model.quant ? model.quant.toUpperCase() : DASH}</Field>
            <Field label="Layers">{count(model.num_layers)}</Field>
            {model.is_moe ? <Field label="Experts">{count(model.num_experts)}</Field> : null}
            <Field label="Max position">{tokens(model.max_position)}</Field>
            <Field label="FTW fingerprint" mono>
              {text(model.ftw_fingerprint)}
            </Field>
            <Field label="Template">{model.template_status.label}</Field>
            <Field label="Converted" mono>
              {text(model.converted_to)}
            </Field>
            <Field label="Served as" mono>
              {model.served_name}
            </Field>
            <Bullets items={model.guidance} />
            <SamplingEditor model={model} />
          </>
        )}
      </Pane>
    </div>
  );
}
