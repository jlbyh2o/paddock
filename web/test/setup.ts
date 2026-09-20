/**
 * Vitest setup, run before each test file's imports are evaluated.
 *
 * uPlot calls `window.matchMedia` while its module loads (a devicePixelRatio
 * shim), and jsdom leaves matchMedia unimplemented, so a bare import throws before
 * any test runs. Stubbing it here — rather than in a per-file `beforeEach`, which
 * runs too late — keeps every chart test, present and future, from tripping over
 * it. The canvas 2D context is only needed at draw time, so that stays in the
 * tests that render a chart.
 */

const mediaList = {
  matches: false,
  media: "",
  onchange: null,
  addListener() {},
  removeListener() {},
  addEventListener() {},
  removeEventListener() {},
  dispatchEvent() {
    return false;
  },
};

Object.defineProperty(globalThis, "matchMedia", {
  writable: true,
  configurable: true,
  value: (query: string) => ({ ...mediaList, media: query }),
});
