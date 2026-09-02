// The keystroke expression linter (DESIGNER-SPEC §5): a thin wrapper over
// `crates/expr-wasm`'s `wasm-bindgen` build, which is `eio-expr` itself —
// the same interpreter code the daemon runs (DESIGNER §1) — compiled to
// WASM. Nothing in this module re-implements parsing, static analysis, or
// error wording; it only initializes the module once and JSON.parses what
// `lint()` returns.
//
// The import below reaches outside `designer/` on purpose: `crates/expr-wasm`
// is built with `wasm-pack build --target web --release` from that crate
// (not from here — this shell owns no Cargo invocation), and its `pkg/`
// output is what this file imports. `pkg/` is gitignored (its own
// `pkg/.gitignore`) and is not committed; a fresh checkout must run that
// build before `npm run dev`/`build`/`test` can succeed. That coupling is
// unavoidable short of vendoring a second copy of the WASM binary into
// `designer/`, which would be a stale copy the moment `eio-expr` changed.
//
// eslint-disable-next-line etc. are not configured in this project; the
// `@ts-expect-error`-free import below resolves because `crates/expr-wasm/pkg`
// carries its own `.d.ts` (wasm-bindgen's generated types), which
// TypeScript follows across the relative path with no project-membership
// requirement.
import init, { lint as wasmLint } from '../../../../crates/expr-wasm/pkg/eio_expr_wasm.js';

/** A byte-offset span into the linted source (EXPR §8). */
export interface LintSpan {
  start: number;
  end: number;
}

/** One diagnostic, in source order. */
export interface LintDiagnostic {
  /** An EXPR §8 error code (`"PARSE"`, `"UNBOUND"`, `"TYPE"`, …). */
  code: string;
  span: LintSpan;
  message: string;
}

/** `lint()`'s result, decoded. Mirrors `crates/expr-wasm/src/lib.rs`'s
 * `LintResult` field for field. */
export interface LintResult {
  /** Whether the expression parses and passes EXPR §10 static analysis. */
  ok: boolean;
  /** EXPR §10's constant-vs-per-signal classification (DESIGNER §5's
   * "signal-dependence badge"). `false` on a parse failure. */
  signal_dependent: boolean;
  diagnostics: LintDiagnostic[];
  /** The `$name`s referenced but unresolved. */
  unbound: string[];
}

let readyPromise: Promise<void> | null = null;
let initialized = false;

/**
 * Initializes the WASM module. Idempotent and safe to call from more than
 * one component — every caller shares the same in-flight/-completed
 * initialization, so the `.wasm` is fetched once per page load regardless
 * of how many expression fields exist on the canvas.
 */
export function ensureLinterReady(): Promise<void> {
  if (!readyPromise) {
    readyPromise = init().then(() => {
      initialized = true;
    });
  }
  return readyPromise;
}

/** Whether {@link ensureLinterReady}'s promise has resolved. */
export function isLinterReady(): boolean {
  return initialized;
}

/**
 * Lints `source` against EXPR §9's reference budgets (ABI §7.1's
 * configure-time gate). Throws if called before {@link ensureLinterReady}'s
 * promise has resolved — callers lint on keystroke, so the expected use is
 * to await readiness once (e.g. in a component's `onMount`) and call this
 * synchronously thereafter, which is what keeps linting fast enough to run
 * on every keystroke without an `await` in the input handler.
 */
export function lintExpression(source: string): LintResult {
  if (!initialized) {
    throw new Error('lintExpression called before ensureLinterReady() resolved');
  }
  return JSON.parse(wasmLint(source)) as LintResult;
}
