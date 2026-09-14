/**
 * Serve — §5.6.
 *
 * The knob *schema* comes from `GET /api/knobs` and the *values* from the snapshot;
 * this file never invents a default, a domain or a help string. Validation is the
 * daemon's too: a value that fails `knobs::validate_value` comes back as a refusal the
 * daemon deliberately did *not* toast (§1.2), because this pane has an inline slot
 * under the field that produced it. That flag — not the status code — is what says the
 * message belongs to a field.
 */

import { useCallback, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { Knob, KnobGroup, ProfileEntry, Snapshot } from "../api/types.ts";
import { ApiError, api } from "../api/client.ts";
import { reportError, run } from "../api/store.ts";
import { exclusions, knobsInGroup, useKnobs } from "../api/knobs.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Empty, Field, Pane } from "../ui/primitives.tsx";
import { basename, count } from "../format.ts";

export function Serve(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const schema = useKnobs();
  const [group, setGroup] = useState<KnobGroup>("model");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [knobErrors, setKnobErrors] = useState<Record<string, string>>({});
  const [showCommand, setShowCommand] = useState(true);
  const [pane, setPane] = useState<"knobs" | "profiles">("knobs");
  const [profileName, setProfileName] = useState("");
  const editorRefs = useRef(new Map<string, HTMLElement>());

  const knobs = useMemo(() => knobsInGroup(schema, group), [schema, group]);
  const groups = schema?.groups ?? [];

  const selection = useSelection(knobs, useCallback((k: Knob) => k.key, []));
  const profiles = useSelection(s.serve.profiles, useCallback((p: ProfileEntry) => p.name, []));

  const clearError = useCallback((key: string) => {
    setKnobErrors((prev) => {
      if (!(key in prev)) return prev;
      const next = { ...prev };
      delete next[key];
      return next;
    });
  }, []);

  const commit = useCallback(
    (knob: Knob, raw: string) => {
      const value = raw.trim() === "" ? null : raw.trim();
      clearError(knob.key);
      api
        .serveKnob({ key: knob.key, value })
        .then(() => {
          setDrafts((prev) => {
            const next = { ...prev };
            delete next[knob.key];
            return next;
          });
        })
        .catch((error: unknown) => {
          // A refusal the daemon left untoasted is this field's to render.
          if (error instanceof ApiError && !error.toasted && !error.isUnauthorized) {
            setKnobErrors((prev) => ({ ...prev, [knob.key]: error.message }));
            return;
          }
          reportError(error);
        });
    },
    [clearError],
  );

  const unset = useCallback(
    (knob: Knob) => {
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[knob.key];
        return next;
      });
      clearError(knob.key);
      run(api.serveKnob({ key: knob.key, value: null }));
    },
    [clearError],
  );

  const activate = useCallback(
    (knob: Knob) => {
      if (knob.kind.kind === "flag") {
        run(api.serveFlag({ key: knob.key }));
        return;
      }
      editorRefs.current.get(knob.key)?.focus();
    },
    [],
  );

  const cycle = useCallback((knob: Knob, delta: number) => {
    run(api.serveCycle({ key: knob.key, delta }));
  }, []);

  const saveProfile = useCallback(() => {
    const name =
      profileName.trim() === "" ? basename(s.serve.values["model"] ?? "") : profileName.trim();
    if (name === "" || name === "—") return;
    run(api.profileSave({ name }), () => {
      setProfileName("");
    });
  }, [profileName, s.serve.values]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (event.key === "Escape") return false;
        if (pane === "profiles") {
          if (profiles.handleKey(event)) return true;
        } else if (selection.handleKey(event)) {
          return true;
        }
        const knob = selection.item;
        switch (event.key) {
          case "ArrowRight":
          case "ArrowLeft": {
            if (groups.length === 0) return false;
            const at = groups.findIndex((g) => g.group === group);
            const step = event.key === "ArrowRight" ? 1 : -1;
            const next = groups[(at + step + groups.length) % groups.length];
            if (next) setGroup(next.group);
            return true;
          }
          case "Enter":
            if (pane === "profiles" && profiles.item) {
              run(api.profileLoad({ name: profiles.item.name }));
              return true;
            }
            if (knob) activate(knob);
            return true;
          case " ":
            if (knob) cycle(knob, 1);
            return true;
          case "x":
          case "Delete":
          case "Backspace":
            if (knob) unset(knob);
            return true;
          case "a":
            run(api.servePlan());
            return true;
          case "p":
            setShowCommand((prev) => !prev);
            return true;
          case "S":
            saveProfile();
            return true;
          case "P":
            if (profiles.item) run(api.profileLoad({ name: profiles.item.name }));
            return true;
          case "D":
            if (profiles.item) run(api.profileDelete({ name: profiles.item.name }));
            return true;
          case "g":
            run(api.engineStart());
            return true;
          default:
            return false;
        }
      },
      [pane, profiles, selection, groups, group, activate, cycle, unset, saveProfile],
    ),
  );

  const serverErrors = new Map(s.serve.errors.map((e) => [e.key, e.message] as const));

  const editorFor = (knob: Knob): ReactNode => {
    const stored = s.serve.values[knob.key];
    const draft = drafts[knob.key];
    const register = (el: HTMLElement | null) => {
      if (el) editorRefs.current.set(knob.key, el);
      else editorRefs.current.delete(knob.key);
    };

    if (knob.kind.kind === "flag") {
      return (
        <label className="toggle">
          <input
            ref={register}
            type="checkbox"
            checked={stored !== undefined}
            // The visible text beside it is the state ("on", "(auto)"), which names the
            // value rather than the knob; without this the checkbox has no usable name.
            aria-label={knob.label}
            onChange={(e) => run(api.serveFlag({ key: knob.key, on: e.target.checked }))}
          />
          <span className={stored === undefined ? "dim" : ""}>
            {stored === undefined ? `(${knob.default})` : "on"}
          </span>
        </label>
      );
    }

    if (knob.kind.kind === "choice") {
      return (
        <select
          ref={register}
          value={stored ?? ""}
          onChange={(e) => commit(knob, e.target.value)}
          aria-label={knob.label}
        >
          <option value="">({knob.default})</option>
          {knob.kind.options.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      );
    }

    if (knob.kind.kind === "multi") {
      // Checkboxes rather than a multi-select list: there are two options, both need their
      // own label, and a list box that scrolls at two rows helps nobody. The stored value
      // is the space-separated argv the flag takes, so the order of `options` is kept
      // rather than the order they were clicked.
      const chosen = new Set((stored ?? "").split(/\s+/).filter(Boolean));
      const options = knob.kind.options;
      return (
        <span className="multi" ref={register} tabIndex={-1}>
          {options.map((option) => (
            <label key={option} className="toggle">
              <input
                type="checkbox"
                checked={chosen.has(option)}
                aria-label={`${knob.label}: ${option}`}
                onChange={(e) => {
                  const next = new Set(chosen);
                  if (e.target.checked) next.add(option);
                  else next.delete(option);
                  commit(knob, options.filter((o) => next.has(o)).join(" "));
                }}
              />
              <span className={chosen.has(option) ? "" : "dim"}>{option}</span>
            </label>
          ))}
          {chosen.size === 0 ? <span className="dim">({knob.default})</span> : null}
        </span>
      );
    }

    const numeric = knob.kind.kind === "int" || knob.kind.kind === "float";
    return (
      <input
        ref={register}
        type={numeric ? "number" : "text"}
        inputMode={numeric ? "decimal" : undefined}
        step={knob.kind.kind === "float" ? "any" : 1}
        min={knob.kind.kind === "int" || knob.kind.kind === "float" ? (knob.kind.min ?? undefined) : undefined}
        max={knob.kind.kind === "int" || knob.kind.kind === "float" ? (knob.kind.max ?? undefined) : undefined}
        value={draft ?? stored ?? ""}
        placeholder={knob.default}
        aria-label={knob.label}
        className={knobErrors[knob.key] ? "error" : ""}
        onChange={(e) => setDrafts((prev) => ({ ...prev, [knob.key]: e.target.value }))}
        onBlur={(e) => {
          if (draft !== undefined && draft !== (stored ?? "")) commit(knob, e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit(knob, e.currentTarget.value);
            e.currentTarget.blur();
          }
          if (e.key === "Escape") {
            setDrafts((prev) => {
              const next = { ...prev };
              delete next[knob.key];
              return next;
            });
            e.currentTarget.blur();
          }
        }}
        style={{ width: "18ch" }}
      />
    );
  };

  const highlighted = selection.item;

  return (
    <div className="stack">
      <div className="panes wide">
        <Pane
          title="Knobs"
          note={schema ? `${count(knobs.length)} in ${group}` : "loading the schema…"}
          bodyClassName="flush"
          actions={
            <>
              {groups.map((g) => (
                <button
                  key={g.group}
                  type="button"
                  className={`btn ${g.group === group ? "primary" : ""}`}
                  onClick={() => setGroup(g.group)}
                >
                  {g.title}{" "}
                  <span className="dim">{count(s.serve.set_counts[g.group] ?? 0)}</span>
                </button>
              ))}
            </>
          }
        >
          {knobs.length === 0 ? (
            <Empty>
              <p>Waiting for the knob schema.</p>
            </Empty>
          ) : (
            <ul className="rows scroll h-560">
              {knobs.map((knob) => {
                const error = knobErrors[knob.key] ?? serverErrors.get(knob.key);
                return (
                  <li
                    key={knob.key}
                    className={`row ${knob.key === selection.id ? "selected" : ""}`}
                    onClick={() => {
                      setPane("knobs");
                      selection.select(knob.key);
                    }}
                  >
                    <div className="row-main">
                      <span className="grow">{knob.label}</span>
                      {editorFor(knob)}
                      <button
                        type="button"
                        className="btn small"
                        title="unset, back to the default"
                        onClick={(e) => {
                          e.stopPropagation();
                          unset(knob);
                        }}
                        disabled={s.serve.values[knob.key] === undefined}
                      >
                        x
                      </button>
                    </div>
                    {error ? <div className="row-sub bad">{error}</div> : null}
                  </li>
                );
              })}
            </ul>
          )}
        </Pane>

        <div className="stack">
          <Pane title="What it does">
            {!highlighted ? (
              <Empty>
                <p>Select a knob.</p>
              </Empty>
            ) : (
              <>
                <Field label="Flag" mono>
                  {highlighted.flag}
                </Field>
                <Field label="Default">{highlighted.default}</Field>
                {highlighted.kind.kind === "choice" ? (
                  <Field label="Options" mono>
                    {highlighted.kind.options.join("  ")}
                  </Field>
                ) : null}
                {highlighted.kind.kind === "int" || highlighted.kind.kind === "float" ? (
                  <Field label="Domain">
                    {highlighted.kind.min ?? "−∞"} … {highlighted.kind.max ?? "∞"}
                  </Field>
                ) : null}
                {exclusions(schema, highlighted).length > 0 ? (
                  <Field label="Excludes" mono>
                    {exclusions(schema, highlighted).join("  ")}
                  </Field>
                ) : null}
                <p>{highlighted.help}</p>
              </>
            )}
          </Pane>

          <Pane
            title="Profiles"
            note={count(s.serve.profiles.length)}
            bodyClassName="flush"
            actions={
              <>
                <input
                  type="text"
                  value={profileName}
                  placeholder={basename(s.serve.values["model"] ?? "")}
                  onChange={(e) => setProfileName(e.target.value)}
                  aria-label="Profile name"
                  style={{ flex: "1 1 140px" }}
                />
                <button type="button" className="btn" onClick={saveProfile}>
                  Save as <span className="dim">(S)</span>
                </button>
                <button
                  type="button"
                  className="btn"
                  disabled={!profiles.item}
                  onClick={() => {
                    if (profiles.item) run(api.profileLoad({ name: profiles.item.name }));
                  }}
                >
                  Load <span className="dim">(P)</span>
                </button>
                <button
                  type="button"
                  className="btn danger"
                  disabled={!profiles.item}
                  onClick={() => {
                    if (profiles.item) run(api.profileDelete({ name: profiles.item.name }));
                  }}
                >
                  Delete <span className="dim">(D)</span>
                </button>
              </>
            }
          >
            {s.serve.profiles.length === 0 ? (
              <Empty>
                <p>
                  No profiles yet. Save the current configuration under a name, then load it
                  back on any later run.
                </p>
              </Empty>
            ) : (
              <ul className="rows scroll h-260">
                {s.serve.profiles.map((profile) => (
                  <li
                    key={profile.name}
                    className={`row ${profile.name === profiles.id ? "selected" : ""}`}
                    onClick={() => {
                      setPane("profiles");
                      profiles.select(profile.name);
                    }}
                    onDoubleClick={() => run(api.profileLoad({ name: profile.name }))}
                  >
                    <div className="row-main">
                      <span className="grow">{profile.name}</span>
                      {profile.name === s.serve.last_used_profile ? (
                        <span className="accent">active</span>
                      ) : null}
                    </div>
                    <div className="row-sub mono">{profile.model ?? "no model set"}</div>
                  </li>
                ))}
              </ul>
            )}
          </Pane>
        </div>
      </div>

      <Pane
        title="Command"
        note={showCommand ? undefined : "hidden (p)"}
        actions={
          <>
            <button type="button" className="btn" onClick={() => setShowCommand((p) => !p)}>
              {showCommand ? "Hide" : "Show"} <span className="dim">(p)</span>
            </button>
            <button type="button" className="btn" onClick={() => run(api.servePlan())}>
              Plan <span className="dim">(a)</span>
            </button>
            <button type="button" className="btn primary" onClick={() => run(api.engineStart())}>
              Start the engine <span className="dim">(g)</span>
            </button>
          </>
        }
      >
        {showCommand ? (
          // A command line is one value, not a log: wrap it rather than making the
          // reader scroll a 120px box sideways to see the flags at the end.
          <pre className="loglist wrap-text" style={{ maxHeight: 140 }}>
            {s.serve.command_preview}
          </pre>
        ) : null}
        {/*
          §2.12: `flag` is the daemon's own resolution of the key, and it is null for a
          key the schema does not know — a profile from a newer FreeToken, or a
          hand-edited profiles.toml. Such a key has no row in the knob list, so this is
          the only place it appears, and it must read as itself.
        */}
        {s.serve.errors.slice(0, 4).map((error) => (
          <div key={`${error.key}-${error.message}`} className="bad mono">
            {error.flag ?? error.key}: {error.message}
          </div>
        ))}
      </Pane>
    </div>
  );
}
