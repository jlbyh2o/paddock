/**
 * The login page, shown when `GET /api/auth` says a token is required and this
 * browser does not have one. `POST /api/login` sets the `paddock_token` cookie,
 * which is what the `EventSource` stream needs — it cannot send a header.
 */

import { useState } from "react";
import type { FormEvent, ReactNode } from "react";
import { ApiError, api } from "../api/client.ts";
import { Pane } from "./primitives.tsx";

export function Login(props: { onAuthorized: () => void }): ReactNode {
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    api
      .login({ token })
      .then((reply) => {
        if (reply.authorized) props.onAuthorized();
        else setError("that token is not correct");
      })
      .catch((cause: unknown) => {
        setError(cause instanceof ApiError ? cause.message : "could not reach the daemon");
      })
      .finally(() => {
        setBusy(false);
      });
  };

  return (
    <div className="login">
      <Pane title="paddock">
        <p className="dim">This instance is protected by a token.</p>
        <form onSubmit={submit}>
          <label htmlFor="token">Token</label>
          <input
            id="token"
            type="password"
            autoFocus
            autoComplete="current-password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            className={error ? "error" : ""}
          />
          {error ? <span className="bad">{error}</span> : null}
          <button type="submit" className="btn primary" disabled={busy || token.length === 0}>
            {busy ? "Signing in…" : "Sign in"}
          </button>
        </form>
      </Pane>
    </div>
  );
}
