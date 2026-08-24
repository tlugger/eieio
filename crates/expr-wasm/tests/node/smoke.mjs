// Loads the wasm-pack `--target web` artifact in a real JS runtime (Node) and
// drives it through real cases, per eieio-m9s.3's verification requirement that
// compiling is not proving it works.
//
// Run: node crates/expr-wasm/tests/node/smoke.mjs
//
// Exits 0 if every assertion passes, 1 otherwise, printing a line per assertion.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import * as pkg from "../../pkg/eio_expr_wasm.js";

const here = fileURLToPath(new URL(".", import.meta.url));
const wasmBytes = readFileSync(`${here}../../pkg/eio_expr_wasm_bg.wasm`);
pkg.initSync({ module: wasmBytes });

let failures = 0;

function assert(name, condition, detail) {
  if (condition) {
    console.log(`ok   - ${name}`);
  } else {
    failures += 1;
    console.log(`FAIL - ${name}${detail ? `: ${detail}` : ""}`);
  }
}

// 1. A valid expression evaluates, through the real interpreter, to the right value.
{
  const result = JSON.parse(pkg.evaluate("(+ 1 2)", null, null));
  assert(
    "valid expression evaluates",
    result.ok === true && result.value?.int === 3,
    JSON.stringify(result),
  );
}

// 2. A syntax error is caught, and its SPAN is the actual byte range — not merely
// "it failed". `(+ 1 2` (6 bytes, indices 0..6) is missing its closing paren;
// `crates/expr/src/parse.rs`'s `parse_list` joins the open paren's span with
// every item parsed and end-of-input, so an unterminated list's span covers the
// whole malformed list — 0..6 here, verified against the real interpreter
// (crates/expr/src/parse.rs, the "unterminated list" branch of `parse_list`),
// not asserted from a guess.
{
  const source = "(+ 1 2";
  const result = JSON.parse(pkg.lint(source));
  const span = result.diagnostics[0]?.span;
  assert(
    "syntax error reports PARSE",
    result.ok === false && result.diagnostics[0]?.code === "PARSE",
    JSON.stringify(result),
  );
  assert(
    "syntax error span is the exact byte offset (0..6, the whole unterminated list)",
    span?.start === 0 && span?.end === 6,
    JSON.stringify(span),
  );
}

// 3. An unbound symbol is named, at its own span.
{
  const source = "(+ 1 frobnicate)";
  const result = JSON.parse(pkg.lint(source));
  assert(
    "unbound symbol is caught by static analysis",
    result.ok === false,
    JSON.stringify(result),
  );
  assert(
    "unbound symbol is named",
    result.unbound.includes("frobnicate"),
    JSON.stringify(result.unbound),
  );
  const diag = result.diagnostics.find((d) => d.code === "UNBOUND");
  const expectedStart = source.indexOf("frobnicate");
  const expectedEnd = expectedStart + "frobnicate".length;
  assert(
    "unbound symbol's span covers exactly its own text",
    diag?.span.start === expectedStart && diag?.span.end === expectedEnd,
    JSON.stringify(diag),
  );
}

// 4. A signal-independent (constant) expression is classified as such.
{
  const result = JSON.parse(pkg.lint("(* 60 1000)"));
  assert(
    "constant expression is signal_dependent: false",
    result.ok === true && result.signal_dependent === false,
    JSON.stringify(result),
  );
  const evaluated = JSON.parse(pkg.evaluate("(* 60 1000)", null, null));
  assert(
    "constant expression evaluates under SIGNAL_NONE",
    evaluated.ok === true && evaluated.value?.int === 60000,
    JSON.stringify(evaluated),
  );
}

// 5. A signal-dependent expression is classified as such, and evaluates against
// a real signal passed across the boundary as tagged JSON.
{
  const result = JSON.parse(pkg.lint("(> $temp $threshold)"));
  assert(
    "signal-dependent expression is signal_dependent: true",
    result.ok === true && result.signal_dependent === true,
    JSON.stringify(result),
  );
  const signal = JSON.stringify({ temp: { float: 21.5 }, threshold: { int: 20 } });
  const evaluated = JSON.parse(pkg.evaluate("(> $temp $threshold)", signal, null));
  assert(
    "signal-dependent expression evaluates against a real signal",
    evaluated.ok === true && evaluated.value?.bool === true,
    JSON.stringify(evaluated),
  );
}

// 6. Missing signal data is an error, not null (EXPR §6) — evaluated for real,
// not merely asserted about the spec.
{
  const evaluated = JSON.parse(
    pkg.evaluate("$humidity", JSON.stringify({ temp: { float: 21.5 } }), null),
  );
  assert(
    "missing attribute is MISSING, not null",
    evaluated.ok === false && evaluated.error?.code === "MISSING",
    JSON.stringify(evaluated),
  );
}

console.log(`\n${failures === 0 ? "ALL PASSED" : `${failures} FAILURE(S)`}`);
process.exit(failures === 0 ? 0 : 1);
