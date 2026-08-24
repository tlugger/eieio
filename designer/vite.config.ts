/// <reference types="vitest/config" />
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
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
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.{test,spec}.{js,ts}'],
  },
})
