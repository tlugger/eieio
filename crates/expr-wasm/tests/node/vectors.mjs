// Cross-checks the WASM build against `expr-tests/`'s host-agnostic conformance
// vectors (EXPR-SPEC §11) — the whole point of shipping the real `eio_expr`
// crate rather than a TypeScript reimplementation. Runs every top-level
// language-suite file (the ones `expr-tests/README.md` says the language
// runner reads): arithmetic, comparison, predicates, strings, collections,
// rendering, forms, signal, semantics, errors, budgets, analysis.
//
// `properties/` and `cbor/` are deliberately excluded: both are separate
// suites for `host-core`/`signal` respectively (properties/ needs a
// `PropertyType` this crate never sees; cbor/ is about the wire encoding),
// and neither is reachable from `eio_expr::analyze`/`eval` alone.
//
// Run: node crates/expr-wasm/tests/node/vectors.mjs
//
// Exits 0 iff every vector agrees with this WASM build. A disagreement is
// reported per-vector and is the finding eieio-m9s.3 exists to surface, not a
// test to be edited into passing (expr-tests/README.md's "Adding vectors").

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import * as pkg from "../../pkg/eio_expr_wasm.js";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = fileURLToPath(new URL("../../../../", import.meta.url));
const wasmBytes = readFileSync(`${here}../../pkg/eio_expr_wasm_bg.wasm`);
pkg.initSync({ module: wasmBytes });

const exprTestsDir = `${repoRoot}expr-tests`;
const files = readdirSync(exprTestsDir)
  .filter((name) => name.endsWith(".json"))
  .sort();

function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return a === b;
  if (typeof a !== "object") return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) {
    if (a.length !== b.length) return false;
    return a.every((item, i) => deepEqual(item, b[i]));
  }
  const aKeys = Object.keys(a).sort();
  const bKeys = Object.keys(b).sort();
  if (aKeys.length !== bKeys.length || aKeys.some((k, i) => k !== bKeys[i])) {
    return false;
  }
  return aKeys.every((k) => deepEqual(a[k], b[k]));
}

let total = 0;
let passed = 0;
const failures = [];

function fail(file, vector, reason) {
  failures.push({ file, name: vector.name, reason });
}

function run(file, vector) {
  total += 1;
  const signalJson = vector.signal ? JSON.stringify(vector.signal) : null;
  const budgetJson = vector.budget ? JSON.stringify(vector.budget) : null;

  // §10 static-analysis facts, checked whenever the vector asserts one.
  const lint = JSON.parse(pkg.lint(vector.expr));

  if ("signal_dependent" in vector) {
    if (lint.signal_dependent !== vector.signal_dependent) {
      fail(
        file,
        vector,
        `signal_dependent: expected ${vector.signal_dependent}, got ${lint.signal_dependent}`,
      );
      return;
    }
  }

  if (vector.error === "ANY") {
    // README: "a vector expecting ANY MUST be rejected by analysis, not merely
    // fail to evaluate."
    if (lint.ok !== false) {
      fail(file, vector, `error: ANY expects static analysis to reject this, but lint.ok was ${lint.ok}`);
      return;
    }
    passed += 1;
    return;
  }

  if (vector.error === "PARSE") {
    // Checked through `evaluate`, not `lint`: `lint` always parses under EXPR
    // §9's reference defaults, but two of budgets.json's PARSE vectors assert
    // rejection under a *tightened* `expr_bytes`/`depth` (clamped up to its
    // floor) — a budget only `evaluate`'s `budget_json` threads down to the
    // parse step. `evaluate` never runs the expression when parsing itself
    // fails, so this is still exactly a parse-time check.
    const evaluated = JSON.parse(pkg.evaluate(vector.expr, signalJson, budgetJson));
    if (evaluated.ok !== false || evaluated.error?.code !== "PARSE") {
      fail(
        file,
        vector,
        `error: PARSE expected, got ${JSON.stringify(evaluated)}`,
      );
      return;
    }
    passed += 1;
    return;
  }

  if (typeof vector.error === "string") {
    // README: "a vector pinning a real code MUST analyse clean" — the fault is
    // an evaluation-time one, so static analysis must find nothing wrong.
    if (lint.ok !== true) {
      fail(
        file,
        vector,
        `error: ${vector.error} expected to analyse clean, but lint.ok was false: ${JSON.stringify(lint.diagnostics)}`,
      );
      return;
    }
    const evaluated = JSON.parse(pkg.evaluate(vector.expr, signalJson, budgetJson));
    if (evaluated.ok !== false || evaluated.error?.code !== vector.error) {
      fail(
        file,
        vector,
        `error: expected ${vector.error}, got ${JSON.stringify(evaluated)}`,
      );
      return;
    }
    passed += 1;
    return;
  }

  if ("expect" in vector) {
    const evaluated = JSON.parse(pkg.evaluate(vector.expr, signalJson, budgetJson));
    if (evaluated.ok !== true) {
      fail(file, vector, `expect: evaluation failed: ${JSON.stringify(evaluated)}`);
      return;
    }
    if (!deepEqual(evaluated.value, vector.expect)) {
      fail(
        file,
        vector,
        `expect: expected ${JSON.stringify(vector.expect)}, got ${JSON.stringify(evaluated.value)}`,
      );
      return;
    }
    // `render` is an independent second assertion pinning `(string result)`'s
    // canonical rendering (EXPR §7.6) — checked when present, by evaluating the
    // vector's own expression wrapped in `(string ...)`, still going through
    // the real interpreter rather than a separate render-only export.
    if ("render" in vector) {
      const rendered = JSON.parse(
        pkg.evaluate(`(string ${vector.expr})`, signalJson, budgetJson),
      );
      if (rendered.ok !== true || rendered.value?.str !== vector.render) {
        fail(
          file,
          vector,
          `render: expected ${JSON.stringify(vector.render)}, got ${JSON.stringify(rendered)}`,
        );
        return;
      }
    }
    passed += 1;
    return;
  }

  // A vector with neither `expect` nor `error` but a `signal_dependent`
  // assertion only (analysis.json has a couple of these) — already checked
  // above, so there is nothing left to run.
  passed += 1;
}

for (const file of files) {
  const data = JSON.parse(readFileSync(`${exprTestsDir}/${file}`, "utf8"));
  for (const vector of data.vectors) {
    run(file, vector);
  }
}

console.log(`${passed}/${total} vectors agreed with the WASM build, across ${files.length} files: ${files.join(", ")}`);

if (failures.length > 0) {
  console.log(`\n${failures.length} DISAGREEMENT(S):`);
  for (const f of failures) {
    console.log(`  [${f.file}] ${f.name}: ${f.reason}`);
  }
}

process.exit(failures.length === 0 ? 0 : 1);
