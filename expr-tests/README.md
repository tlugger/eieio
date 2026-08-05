# expr-tests — the expression language conformance suite

Host-agnostic vectors for the eieio expression language (EXPR-SPEC §11). Every
interpreter deployment — the daemon, the leaf runtime, `expr` compiled to WASM for the
Designer's editor, and any host written later in any language — MUST pass all of them
identically. **Divergence between two hosts is a conformance bug by definition**, and
the fix is never to make one host special.

These are data files, not Rust. That is the whole point: a suite written in Rust could
only ever test the Rust implementation.

## Layout

One file per area of the specification. Each is a JSON object with a `vectors` array.

|File|Covers|
|---|---|
|`arithmetic.json`|§7.1 arithmetic|
|`comparison.json`|§7.2 comparison and logic|
|`predicates.json`|§7.3 type predicates and conversion|
|`strings.json`|§7.4 strings|
|`collections.json`|§7.5 collections|
|`rendering.json`|§7.6 canonical rendering — the pins|
|`forms.json`|§5 special forms|
|`signal.json`|§6 signal access|
|`semantics.json`|§4.1 truthiness, §4.2 equality|
|`errors.json`|§8 error codes|
|`budgets.json`|§9 bounds, at the floors|
|`analysis.json`|§10 static analysis, signal-dependence classification|

## A vector

```json
{
  "name": "add-mixed-promotes-to-float",
  "expr": "(+ 1 2.5)",
  "expect": { "float": 3.5 },
  "spec": "§7.1",
  "note": "int + float promotes; the spec's rule, not the implementation's choice"
}
```

|Field|Required|Meaning|
|---|---|---|
|`name`|yes|Unique within the file. Appears in failure output, so name the behaviour, not the input.|
|`expr`|yes|The expression source, exactly as a user would write it.|
|`expect`|one of|The [value](#values) the expression must evaluate to.|
|`error`|one of|The EXPR §8 error code it must fail with, spelled as §8 spells it: `PARSE`, `UNBOUND`, `TYPE`, `ARITY`, `DOMAIN`, `NO_SIGNAL`, `MISSING`, `FUEL`, `DEPTH`, `SIZE`. Or `ANY` — see below.|
|`signal`|no|The signal to evaluate against, as an object of attribute name → [value](#values). Absent means `SIGNAL_NONE` — no signal context (ABI §7.1).|
|`render`|no|What `(string result)` must produce — the §7.6 canonical rendering of this vector's value. An independent second assertion: `expect` pins the value, `render` pins how every host writes it down.|
|`signal_dependent`|no|Asserts the static signal-dependence classification of `expr` (§10). Independent of evaluation: a vector may assert it with or without a `signal`.|
|`budget`|no|Budget overrides, for vectors about §9. Any subset of `fuel`, `depth`, `range`, `value_bytes`, `expr_bytes`. Omitted knobs take the §9 reference defaults, and a host clamps any value below its §9 floor *up* — so a vector asking for `{"fuel": 1}` is asserting behaviour at the floor, not at 1.|
|`spec`|no|The section this vector comes from. Documentation for the reader.|
|`note`|no|Why this vector exists, when that is not obvious.|

`"error": "ANY"` asserts that the expression is **rejected**, without pinning which code
says so. It exists because §10 makes exactly that distinction: for a statically-invalid
expression — `(let (x) x)`, a parameter shadowing a special form, an empty list — hosts
MUST agree on *whether* it is rejected, and "which code a host attaches to each rejection
is diagnostic rather than normative". A vector pinning a code there would fail a
conforming host. Use a real code everywhere else; `ANY` is a statement about the spec, not
a way to avoid deciding.

Exactly one of `expect` and `error` must be present. A vector with neither, both, or an
unknown field is malformed and the runner rejects the file — a typo'd field name that was
silently ignored would be a vector that asserts nothing.

## Values

A value is a JSON object with exactly one key, naming its type in the §2 data model. The
tag is not decoration: `1` and `1.0` are different values (§4.2 compares them equal but
`=` is not identity), and JSON cannot tell them apart on its own.

|Type|Notation|Notes|
|---|---|---|
|null|`{"null": null}`||
|bool|`{"bool": true}`||
|int|`{"int": -7}`|Within `i64` (§2).|
|float|`{"float": 21.5}`|Finite binary64. `{"float": 3}` is `3.0` — the tag decides, not the spelling. Negative zero is `{"float": -0.0}`.|
|string|`{"str": "abc"}`|Any Unicode scalar sequence.|
|bytes|`{"bytes": "61ff"}`|Lowercase hex, two digits per byte. `{"bytes": ""}` is empty.|
|array|`{"arr": [{"int": 1}, {"str": "a"}]}`|Elements are values, in order.|
|map|`{"map": {"a": {"int": 1}}}`|Keys are strings; iteration order is by key (§2), not by file order.|

There is no notation for a function: a function cannot be an expression's result (§2), so
no vector can expect one. A vector whose expression would produce one asserts
`"error": "TYPE"`.

Floats that JSON cannot spell exactly are the one place this notation reaches its limit.
None are needed: every float in the suite is either exactly representable in decimal or is
a documented boundary (`5e-324`, `1.7976931348623157e308`) that round-trips through JSON's
number grammar.

## Coverage

The suite is exhaustive by spec mandate (§11), and the runner enforces that rather than
trusting it: `crates/expr/tests/vectors.rs` walks the builtin table, the special-form
list, and the §8 error codes, and fails if any of them has no vector. Adding a builtin
without a vector breaks the build, which is the intended outcome.

`RESULT_TYPE` is the one exempt error code. It is the *host* checking an evaluated value
against the manifest's declared property type (ABI §7.1, §11), not an interpreter
outcome — no expression can produce it, so no vector here can. It gets its vectors where
the behaviour lives, with the host's property-evaluation protocol.

## Adding vectors

Derive the expected result from the **specification**, then run the suite. If the
implementation disagrees, one of the two is wrong and that is a finding to resolve — never
edit the vector to match what the code did. A conformance suite written by observing the
implementation asserts only that the implementation is self-consistent.
