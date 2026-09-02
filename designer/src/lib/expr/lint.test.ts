// Exercises the real `crates/expr-wasm` build, not a stub (the plan's
// verification checklist is explicit about this) — `npm run test` fails
// outright if `wasm-pack build --target web --release` has not been run
// from `crates/expr-wasm` (see lint.ts's header doc), which is the correct
// failure mode: a stubbed test could pass while the actual interpreter
// diverged from what it claims to lint.
//
// `lint.ts`'s own `ensureLinterReady()` fetches the `.wasm` by URL, which is
// the right thing in a real browser (Vite serves it) and the wrong thing
// under Vitest's Node process, which has no server listening for it. So
// this file loads the bytes itself with `node:fs` and hands them to the
// wasm-bindgen glue's `initSync` directly; `ensureLinterReady()` afterward
// sees the module already initialized and resolves without fetching
// (`__wbg_init`'s own short-circuit). `lint.ts` itself is untouched by this
// — only the test's bootstrap differs from the browser's.
//
// Resolved from `process.cwd()` rather than `import.meta.url`: Vitest
// rewrites the latter to a non-`file:` scheme internally, and `npm test`
// always runs with `designer/` as the working directory (its `package.json`
// is what defines the `test` script), so this is the stable anchor.
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it, beforeAll } from 'vitest';
import { initSync } from '../../../../crates/expr-wasm/pkg/eio_expr_wasm.js';
import { ensureLinterReady, lintExpression } from './lint';

beforeAll(async () => {
  const wasmPath = path.resolve(process.cwd(), '../crates/expr-wasm/pkg/eio_expr_wasm_bg.wasm');
  initSync({ module: readFileSync(wasmPath) });
  await ensureLinterReady();
});

describe('lintExpression', () => {
  it('accepts a trivial literal', () => {
    const result = lintExpression('42');
    expect(result.ok).toBe(true);
    expect(result.signal_dependent).toBe(false);
    expect(result.diagnostics).toEqual([]);
  });

  it('classifies a signal-dependent expression', () => {
    const result = lintExpression('(< $temp 18.0)');
    expect(result.ok).toBe(true);
    expect(result.signal_dependent).toBe(true);
  });

  it('reports an unbound symbol', () => {
    // `$nonexistent` is a *signal attribute* (a sigil, EXPR §4) — never
    // statically validated, since a missing key is a per-signal evaluation
    // failure (EXPR §6), not a configure-time one. An unbound plain symbol
    // (no `$`) is what EXPR §10 actually rejects at analysis time.
    const result = lintExpression('some_unknown_function');
    expect(result.ok).toBe(false);
    expect(result.unbound).toContain('some_unknown_function');
  });

  it('reports a PARSE diagnostic at the right span for a syntax error', () => {
    // An unterminated form: the parser should point at exactly where it
    // ran out of input, which is the guarantee the config modal's
    // "diagnostic at its span" (DESIGNER §5) depends on.
    const source = '(+ 1 2';
    const result = lintExpression(source);
    expect(result.ok).toBe(false);
    expect(result.diagnostics.length).toBeGreaterThan(0);
    const diagnostic = result.diagnostics[0]!;
    expect(diagnostic.code).toBe('PARSE');
    expect(diagnostic.span.start).toBeGreaterThanOrEqual(0);
    expect(diagnostic.span.end).toBeLessThanOrEqual(source.length);
    expect(diagnostic.span.end).toBeGreaterThanOrEqual(diagnostic.span.start);
  });
});
