// Component-test environment shims. jsdom (the environment `vite.config.ts`'s `test.environment`
// selects) implements neither of the browser APIs below; nothing in `src/` calls them directly —
// they are pulled in transitively by `@xyflow/svelte`, which `BlockCard.svelte` is registered
// against as a node type (`ServiceCanvas.svelte`) and which a component test for `BlockCard`
// therefore has to mount through (`Handle` needs the real `SvelteFlow` node context — see
// `BlockCard.test.ts`'s own doc comment for why). Neither shim changes what a component asserts;
// both are here only so `SvelteFlow`'s own construction does not throw before a test's assertions
// run.
if (typeof window !== 'undefined' && typeof window.matchMedia !== 'function') {
  // `svelte/reactivity`'s `MediaQuery` (used by `@xyflow/svelte`'s store for a
  // prefers-color-scheme-style check) calls this unconditionally at store construction.
  window.matchMedia = (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }) as unknown as MediaQueryList;
}

if (typeof window !== 'undefined' && typeof window.ResizeObserver !== 'function') {
  // `@xyflow/svelte`'s `NodeRenderer` already guards a missing `ResizeObserver` (`typeof
  // ResizeObserver === 'undefined'`) and degrades to no dimension tracking, which is fine for a
  // mount-only assertion — this shim exists only for any other call site that isn't guarded.
  class NoopResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  window.ResizeObserver = NoopResizeObserver as unknown as typeof ResizeObserver;
}
