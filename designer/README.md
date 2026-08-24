# eieio Designer

The visual management surface for eieio — see `docs/specs/DESIGNER-SPEC.md`
for what this is and why. This package is the frontend half only: a Vite +
Svelte 5 SPA using `@xyflow/svelte` for the canvas. The backend
(`crates/designer`, an `axum` binary) serves the built output of
`npm run build` (which writes `dist/`) and proxies `/api` to it.

**Status:** the app shell only (eieio-m9s.1) — icon rail, one indented
System → Node → Service navigator, the canvas, the block library overlay,
and the block card. Editing (dragging blocks in, drawing connections, the
config modal) is a later issue.

## Structure

```
src/
  lib/
    api/          types.ts (the data model), client.ts (the one seam to the
                   backend), mock.ts (stands in for crates/designer, which
                   doesn't exist yet — see client.ts's header comment)
    derive/        the two derived-value rules (abbreviation, colour) and
                    their unit tests, plus a capability-mismatch helper
    stores/        theme preference (system/light/dark)
    components/    IconRail, NavigatorTree, Toolbar, ServiceCanvas,
                    BlockCard, BlockLibrary
  App.svelte       the shell composition
```

## Commands

- `npm run dev` — dev server with mock data, no backend required
- `npm run build` — production build to `dist/` (the path `crates/designer`
  embeds — do not change it)
- `npm run check` — svelte-check + tsc
- `npm run test` — vitest (unit tests for the derived-value rules)

## The mock boundary

`src/lib/api/client.ts` is the only module allowed to know whether it's
talking to `mock.ts` or the real backend. Swapping to `fetch('/api/...')`
calls against `crates/designer` is a change confined to that one file.
