/// <reference types="vitest/config" />
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
  // Svelte 5 ships two builds behind package.json export conditions:
  // `browser` (the real client runtime, where `mount`/`unmount` work) and a
  // `default` that falls back to the server-side rendering build, which
  // resolves fine but silently no-ops `mount` in a component test. Vite's
  // dev server and build already request the `browser` condition (it's a
  // client-only SPA — DESIGNER §1), so this only changes what Vitest's
  // Node-hosted test runner resolves. Gated on `process.env.VITEST` (set by
  // Vitest itself) rather than applied unconditionally, so `vite build`'s
  // module resolution — and therefore `designer/dist`, which
  // `crates/designer` embeds byte for byte — cannot be touched by a change
  // that exists only to make component tests possible.
  resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
  build: {
    // The Rust backend (crates/designer, built by another agent) embeds
    // exactly this path via rust-embed. Do not change it (DESIGNER §1).
    outDir: 'dist',
  },
  server: {
    proxy: {
      // Points at wherever crates/designer (the axum backend, built in a
      // parallel worktree) listens in development. It does not exist in
      // this worktree, so this proxy path is unexercised here — the app
      // runs against the mock API layer (src/lib/api/mock.ts) until it does.
      '/api': {
        target: 'http://127.0.0.1:8787',
        changeOrigin: true,
      },
    },
    fs: {
      // `lib/expr/lint.ts` imports `crates/expr-wasm/pkg` (DESIGNER §5,
      // §1's "expr crate compiled to WASM"), which sits outside this
      // package's root — Vite's dev server otherwise 403s a request for
      // anything outside it (`server.fs.strict`'s default). `npm run
      // build`'s bundling is unaffected either way (module resolution at
      // build time isn't subject to this dev-only guard); only `npm run
      // dev` needs it.
      allow: ['..'],
    },
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.{test,spec}.{js,ts}'],
    setupFiles: ['./vitest-setup.ts'],
  },
})
