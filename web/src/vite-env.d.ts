/// <reference types="vite/client" />

/**
 * True in a mock build (`VITE_MOCK=1 npm run dev`) and under vitest; false in a
 * production build, where `vite.config.ts` replaces it with the literal `false` so
 * Rollup folds every `import("./mock/…")` away and the fixture never ships.
 */
declare const __PADDOCK_MOCK__: boolean;
