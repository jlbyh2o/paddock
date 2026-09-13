import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

/**
 * The dev server proxies `/api` to a locally running `ft-man web`.
 *
 * `GET /api/events` is a Server-Sent Events stream, so two things matter:
 *  - `ws: false`, because it is plain HTTP and must not be upgraded;
 *  - the request must not invite a compressed or buffered response. Asking for
 *    `identity` encoding and setting `X-Accel-Buffering: no` keeps every frame
 *    flowing through the proxy the moment the daemon writes it, which is what
 *    `EventSource` needs to fire `onmessage` in development.
 *
 * `__FT_MAN_MOCK__` is a compile-time constant rather than a runtime env lookup so
 * that a production build folds `if (__FT_MAN_MOCK__)` to `false` and drops the mock
 * daemon, the fixture and every host path in it. `mode` is `"test"` under vitest,
 * `"production"` for `npm run build`, so the render tests keep their fixture and the
 * shipped bundle carries no `assets/server-*.js`.
 */
export default defineConfig(({ mode }) => {
  const mock = process.env["VITE_MOCK"] === "1" || mode === "test";
  return {
    plugins: [react()],
    define: { __FT_MAN_MOCK__: JSON.stringify(mock) },
    build: {
      outDir: "dist",
      emptyOutDir: true,
      target: "es2022",
      reportCompressedSize: true,
    },
    server: {
      port: 5173,
      proxy: {
        "/api": {
          target: "http://127.0.0.1:7979",
          changeOrigin: false,
          ws: false,
          secure: false,
          headers: {
            "Accept-Encoding": "identity",
            "X-Accel-Buffering": "no",
            "Cache-Control": "no-cache",
          },
        },
      },
    },
    test: {
      environment: "jsdom",
      globals: false,
      include: ["src/**/*.test.{ts,tsx}"],
      css: false,
      // The render tests drive the fixture rather than a daemon, exactly as
      // `VITE_MOCK=1 npm run dev` does.
      env: { VITE_MOCK: "1" },
    },
  };
});
