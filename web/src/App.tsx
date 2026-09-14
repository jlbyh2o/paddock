/**
 * The chrome: tab bar, status pill, footer hints, toasts, the confirmation modal,
 * the plan overlay, the help overlay, and the connection banner.
 *
 * State arrives one way — a `Snapshot` per SSE frame — and leaves one way, as a
 * POST. There is no client-side model of the engine: the status text, the badges,
 * the confirmation body and every number below are the daemon's words.
 */

import { Suspense, useCallback, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { ApiError, MOCK, api, onUnauthorized } from "./api/client.ts";
import { openEventStream } from "./api/events.ts";
import {
  applySnapshot,
  pushLocalToast,
  reportError,
  resetFeeds,
  run,
  setConnected,
  useConnection,
  useSnapshot,
} from "./api/store.ts";
import { loadKnobs } from "./api/knobs.ts";
import { TABS, tabFromHash } from "./tabs/index.ts";
import type { TabId } from "./tabs/index.ts";
import { ConfirmModal } from "./ui/ConfirmModal.tsx";
import { HelpOverlay } from "./ui/HelpOverlay.tsx";
import { Login } from "./ui/Login.tsx";
import { PlanOverlay } from "./ui/PlanOverlay.tsx";
import { Toasts } from "./ui/Toasts.tsx";
import { dispatchTabKey, isPlain, isTypingTarget } from "./ui/keys.ts";
import { THEME_LABEL, useTheme } from "./ui/theme.ts";
import { count } from "./format.ts";

type AuthState = "checking" | "required" | "ok";

function useHashTab(): [TabId, (id: TabId) => void] {
  const [tab, setTab] = useState<TabId>(() => tabFromHash(window.location.hash));

  useEffect(() => {
    const onHashChange = () => {
      setTab(tabFromHash(window.location.hash));
    };
    window.addEventListener("hashchange", onHashChange);
    return () => {
      window.removeEventListener("hashchange", onHashChange);
    };
  }, []);

  const go = useCallback((id: TabId) => {
    window.location.hash = `#${id}`;
    setTab(id);
  }, []);

  return [tab, go];
}

export function App(): ReactNode {
  const [auth, setAuth] = useState<AuthState>("checking");
  const [tab, goTab] = useHashTab();
  const [helpOpen, setHelpOpen] = useState(false);
  const snapshot = useSnapshot();
  const connection = useConnection();
  const theme = useTheme();

  // ---- auth -------------------------------------------------------------
  const checkAuth = useCallback(() => {
    api
      .auth()
      .then((status) => {
        setAuth(status.auth_required && !status.authorized ? "required" : "ok");
      })
      .catch((error: unknown) => {
        // The daemon may simply be down; assume no auth and let the stream retry.
        if (error instanceof ApiError && error.status === 401) setAuth("required");
        else setAuth("ok");
      });
  }, []);

  useEffect(() => {
    checkAuth();
    onUnauthorized(() => {
      setAuth("required");
    });
    return () => {
      onUnauthorized(null);
    };
  }, [checkAuth]);

  // ---- the stream -------------------------------------------------------
  useEffect(() => {
    if (auth !== "ok") return;
    let live = true;
    // First paint does not wait for the stream's first frame — but the stream is the
    // authority. `applySnapshot` drops a document that is not newer than the one
    // already held (§2.2), so this GET losing the race simply does nothing instead of
    // rewinding the page to a state the reader has already seen past.
    api.snapshot().then(
      (snapshot) => {
        if (live) applySnapshot(snapshot);
      },
      () => {
        // The stream is the real source; a failure here is not worth a toast.
      },
    );
    loadKnobs().catch((error: unknown) => {
      reportError(error);
    });

    const close = openEventStream({
      onSnapshot: applySnapshot,
      onReset: () => {
        resetFeeds();
        pushLocalToast("the daemon restarted — reloading state", "warn");
      },
      onConnected: () => {
        setConnected(true);
      },
      onDisconnected: () => {
        setConnected(false);
      },
      onUnauthorized: () => {
        setAuth("required");
      },
    });
    return () => {
      live = false;
      close();
    };
  }, [auth]);

  const confirm = snapshot?.confirm ?? null;
  const plan = snapshot?.serve.plan ?? null;

  const answerConfirm = useCallback((accept: boolean) => {
    run(api.confirm({ accept }));
  }, []);

  // ---- global keys ------------------------------------------------------
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        if (helpOpen) {
          setHelpOpen(false);
          event.preventDefault();
          return;
        }
        if (confirm) {
          answerConfirm(false);
          event.preventDefault();
          return;
        }
        if (plan) {
          run(api.servePlanDismiss());
          event.preventDefault();
          return;
        }
        if (dispatchTabKey(event)) {
          event.preventDefault();
          return;
        }
        if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
        return;
      }

      if (!isPlain(event)) return;

      // A modal owns the keyboard while it is up.
      if (confirm) {
        if (event.key === "Enter") {
          // Enter activates whatever has focus, which is the default (safe) option
          // until the reader moves it. Accepting unconditionally would turn a Tab away
          // from Confirm into a trap: the button says Cancel and the key does the
          // opposite. `y` and `n` stay unambiguous, which is why they are separate.
          const focused = document.activeElement;
          const answer =
            focused instanceof HTMLElement && focused.dataset["confirm"] !== undefined
              ? focused.dataset["confirm"] === "accept"
              : confirm.default_index === 1;
          answerConfirm(answer);
          event.preventDefault();
        } else if (event.key === "y" || event.key === "Y") {
          answerConfirm(true);
          event.preventDefault();
        } else if (event.key === "n" || event.key === "N") {
          answerConfirm(false);
          event.preventDefault();
        }
        return;
      }
      if (plan) {
        if (event.key === "A") {
          run(api.servePlanApply());
          event.preventDefault();
        } else if (event.key === "a" || event.key === "q") {
          run(api.servePlanDismiss());
          event.preventDefault();
        }
        return;
      }
      if (helpOpen) return;

      if (isTypingTarget(event.target)) return;

      if (event.key === "?" || event.key === "F1") {
        setHelpOpen(true);
        event.preventDefault();
        return;
      }
      if (event.key >= "1" && event.key <= "9") {
        const index = Number(event.key) - 1;
        const target = TABS[index];
        if (target) {
          goTab(target.id);
          event.preventDefault();
        }
        return;
      }
      if (dispatchTabKey(event)) event.preventDefault();
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [helpOpen, confirm, plan, answerConfirm, goTab]);

  const active = useMemo(() => TABS.find((t) => t.id === tab) ?? TABS[0], [tab]);

  if (auth === "checking") {
    return <div className="empty" style={{ padding: 24 }}>Connecting…</div>;
  }

  if (auth === "required") {
    return (
      <Login
        onAuthorized={() => {
          setAuth("ok");
        }}
      />
    );
  }

  if (!active) return null;

  const engine = snapshot?.engine ?? null;
  const activeWork = engine ? engine.active_jobs + engine.active_downloads : 0;
  const Body = active.Component;

  return (
    <div className="app">
      <header className="topbar">
        <span className="brand">paddock</span>
        <nav className="tabs" aria-label="Views">
          {TABS.map((def, i) => {
            let badge: number | null = null;
            if (snapshot) {
              if (def.id === "jobs" && activeWork > 0) badge = activeWork;
              if (def.id === "models" && snapshot.models.items.length > 0)
                badge = snapshot.models.items.length;
              if (def.id === "templates" && snapshot.templates.stored.length > 0)
                badge = snapshot.templates.stored.length;
            }
            return (
              <button
                key={def.id}
                type="button"
                className="tab"
                aria-current={def.id === tab ? "page" : undefined}
                onClick={() => goTab(def.id)}
              >
                <span className="key">{i + 1}</span>
                <span>{def.title}</span>
                {badge === null ? null : (
                  <span className={`badge ${def.id === "jobs" ? "active" : ""}`}>
                    {count(badge)}
                  </span>
                )}
              </button>
            );
          })}
        </nav>
        <span className="status-pill" title={engine?.command_line ?? undefined}>
          <span className={`dot ${engine?.status_class ?? "dim"}`} />
          <span className="truncate">
            {engine?.status_text ?? "connecting…"} · {engine?.model ?? "no model"}
          </span>
        </span>
        <button type="button" className="theme-toggle" onClick={theme.cycle}>
          {THEME_LABEL[theme.choice]}
        </button>
        <button type="button" className="theme-toggle" onClick={() => setHelpOpen(true)}>
          ? keys
        </button>
      </header>

      {connection.everConnected && !connection.connected ? (
        <div className="disconnected" role="alert">
          Disconnected from paddock — retrying. The numbers below are the last state seen.
        </div>
      ) : null}
      {MOCK ? (
        <div className="disconnected" style={{ background: "var(--warn)" }}>
          Mock mode: this page is driven by a fixture, not by a running daemon.
        </div>
      ) : null}

      <main className="main">
        {snapshot ? (
          <Suspense fallback={<div className="empty">Loading {active.title}…</div>}>
            <Body snapshot={snapshot} />
          </Suspense>
        ) : (
          <div className="empty">Waiting for the first snapshot…</div>
        )}
      </main>

      <footer className="footer">
        <div className="hints">
          {active.hints.map((hint) => (
            <span className="hint" key={hint.k}>
              <kbd>{hint.k}</kbd>
              {hint.what}
            </span>
          ))}
          <span className="hint">
            <kbd>?</kbd>keys
          </span>
        </div>
        <span className="version">{snapshot?.version ? `v${snapshot.version}` : ""}</span>
      </footer>

      <Toasts toasts={snapshot?.toasts ?? []} />
      {confirm ? <ConfirmModal confirm={confirm} onAnswer={answerConfirm} /> : null}
      {plan && !confirm ? (
        <PlanOverlay
          plan={plan}
          onApply={() => run(api.servePlanApply())}
          onDismiss={() => run(api.servePlanDismiss())}
        />
      ) : null}
      {helpOpen ? (
        <HelpOverlay
          onClose={() => {
            setHelpOpen(false);
          }}
        />
      ) : null}
    </div>
  );
}
