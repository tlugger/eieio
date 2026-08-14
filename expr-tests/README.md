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

`properties/` is a **second suite** in the same directory, and a different question: not
what an expression evaluates to, but whether that value satisfies the property type a
manifest declared, and what a guest decodes when it does (ABI §7.1, §11.1). Its vectors
carry a `type` field the language runner would reject, so it is a subdirectory rather than
another file — the language runner reads top-level files only. `crates/host-core/tests/`
runs it, because `host-core` is the only crate that depends on both halves of the rule:
`expr` evaluates and `manifest`'s `PropertyType` decides.

|File|Covers|
|---|---|
|`properties/types.json`|ABI §11.1 property types: what each satisfies, the int → float promotion and its exactness boundaries, and `RESULT_TYPE`|

`cbor/` is a **third suite**, and further from the expression language still: it is the
canonical encoding of a batch on the wire (ABI §6.3.1), which every host must agree on
byte for byte before anything above it can be compared. It is here rather than in a
directory of its own for the reason the whole tree exists — these are the platform's
host-agnostic vectors, and a host in another language reads them all from one place —
and it is a subdirectory for the same mechanical reason `properties/` is: its vectors
carry fields the language runner would reject, and that runner reads top-level files
only. `crates/signal/tests/cbor_vectors.rs` runs it, `eio_signal` being the one crate
that defines the encoding. Its format is [below](#a-cbor-vector).

|File|Covers|
|---|---|
|`cbor/batches.json`|ABI §6.3.1: batches that decode, and re-encode to the bytes they came from|
|`cbor/reject.json`|Bytes each of the eleven rules refuses|
|`cbor/deviations.json`|The two departures from RFC 8949 §4.2.1, in both directions|

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

A vector in `properties/` uses the same fields with two differences: it carries a required
`type` — the declared property type, spelled as ABI §11.1 spells it (`bool`, `int`, `float`,
`string`, `bytes`, `any`) — and its `expect` is the value a **guest decodes**, after the
type check and any promotion. `{"type": "float", "expr": "22", "expect": {"float": 22}}` is
the promotion asserted end to end: an int expression, a float property, and a float on the
wire. The only `error` it admits is `RESULT_TYPE`; `budget`, `render` and `signal_dependent`
are the language suite's and are not accepted there.

`"error": "ANY"` asserts that the expression is rejected by **§10 static analysis**,
without pinning which code says so. It exists because §10 makes exactly that distinction:
for a statically-invalid expression — `(let (x) x)`, a parameter shadowing a special form,
an empty list — hosts MUST agree on *whether* it is rejected, and "which code a host
attaches to each rejection is diagnostic rather than normative". A vector pinning a code
there would fail a conforming host.

Which gate rejects it is not a detail the corpus leaves open, and the runner enforces the
correspondence in **both** directions:

- a vector expecting `ANY` MUST be rejected by analysis, not merely fail to evaluate.
  Without this, `ANY` degrades into "errors somehow", and every static vector in the
  corpus passes on a host that implements none of §10 — which is what they all did until
  eieio-s85.10;
- a vector pinning a real code MUST analyse clean. A pinned code on a statically-rejected
  expression asserts a code §10 calls diagnostic, so a conforming host that words its
  diagnostics differently would fail this suite.

So the choice is forced, not stylistic: if §10 rejects the expression it is `ANY`, and
otherwise it is the §8 code evaluation produces. Two codes are consequently unreachable as
pinned vectors — `RESULT_TYPE`, which is the host's property check and lives in
`properties/`, and `UNBOUND`, since every unbound symbol is statically decidable and §10
item 3 makes rejecting one a MUST.

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

## A CBOR vector

`cbor/`'s vectors are about bytes rather than expressions, so they share the [value
notation](#values) and nothing else.

```json
{
  "name": "negative-zero-is-preserved",
  "bytes": "81a1617afb8000000000000000",
  "expect": [{ "z": { "float": -0.0 } }],
  "rule": [6],
  "spec": "§6.3.1 rule 6"
}
```

|Field|Required|Meaning|
|---|---|---|
|`name`|yes|Unique across the whole `cbor/` corpus, not merely within its file.|
|`bytes`|yes|The encoded batch, lowercase hex, two digits per byte. `""` is a legal input — and is not the empty batch, which is `"80"`.|
|`expect`|one of|The batch these bytes decode to: an array of signals, each an object of attribute name → [value](#values). `[]` is the empty batch.|
|`reject`|one of|`true`, asserting the bytes are refused. It carries **no reason** — see below.|
|`rule`|yes|Which of §6.3.1's eleven rules this vector exercises, as an array of numbers. Read by the coverage audit and by people; never asserted.|
|`depth`|no|The nesting bound to decode under, for vectors about rule 9. A host clamps a request below EXPR §9's `MAX_DEPTH` floor *up*, so `1` asserts behaviour at the floor rather than at 1.|
|`spec`, `note`|no|Documentation for the reader, as in the language suite.|

Exactly one of `expect` and `reject` must be present, and an unknown field is a malformed
file — the same rule, for the same reason, as the language corpus.

**A rejecting vector says only that the bytes are refused.** It cannot say why, because
§6.3.1 does not let it: "which rule a host rejects under is diagnostic, not normative…
a conformance suite MUST NOT require identical rejection reasons". Two hosts must agree on
*whether* input is canonical and need not agree on how they classify a violation, so a
vector naming an error would fail a conforming host that words its diagnostics differently.
`rule` records which rule the vector is *about*, which is a statement about the corpus
rather than about any host's output.

**Every accepting vector also asserts that re-encoding reproduces `bytes` exactly.** That
is §6.3.1's own requirement — "for every input a decoder accepts, re-encoding the decoded
batch MUST reproduce that input byte for byte" — and it is the half that catches a decoder
which accepts the right values and normalises them on the way in. Negative zero is the
sharpest case: `-0.0` and `+0.0` compare equal under IEEE semantics, so a decoder that
normalised one to the other would satisfy any value assertion and fail only here.

**The two deviations get vectors in both directions.** For each, the corpus carries the
encoding this platform accepts *and* the encoding RFC 8949 §4.2.1 mandates, which this
platform must refuse. A suite carrying only the first would be passed by a host built on a
stock canonical-CBOR library, which is the exact failure the deviations exist to make
visible: shortest-float would write `1.5` as `f93e00`, and encoded-bytes key ordering would
put `"z"` before `"aa"`.

## Coverage

The suite is exhaustive by spec mandate (§11), and the runner enforces that rather than
trusting it: `crates/expr/tests/vectors.rs` walks the builtin table, the special-form
list, and the §8 error codes, and fails if any of them has no vector. Adding a builtin
without a vector breaks the build, which is the intended outcome.

`RESULT_TYPE` is the one error code exempt from *that* audit. It is the *host* checking an
evaluated value against the manifest's declared property type (ABI §7.1, §11), not an
interpreter outcome — no expression can produce it, so no vector in the language files can.
It is covered instead by `properties/`, which has an audit of its own: every ABI §11.1
property type must have a vector showing what satisfies it and, `any` excepted, one showing
what it refuses. Adding a property type without vectors breaks the build too.

`cbor/` is audited the same way, in both directions: every one of §6.3.1's eleven rules
must have a vector it accepts and a vector it refuses, and each of the two RFC 8949
departures must have both forms. **Rule 6 is the one exemption**, and cannot be otherwise:
it mandates that negative zero is *preserved*, so there are no bytes it forbids and no
rejecting vector to write. What would catch a host breaking it is the re-encode assertion
on its accepting vector, which is why that assertion is not optional.

## Adding vectors

Derive the expected result from the **specification**, then run the suite. If the
implementation disagrees, one of the two is wrong and that is a finding to resolve — never
edit the vector to match what the code did. A conformance suite written by observing the
implementation asserts only that the implementation is self-consistent.

In `cbor/`, one step more, because a rejecting vector asserts only *that* bytes are refused
and so passes whatever it is refused for. **Check by hand that the bytes are well-formed
CBOR and that the rule the vector names is the only thing wrong with them.** The failure
this prevents is silent and permanent: a vector for rule 8 arrived in the first corpus with
a text head claiming 24 bytes and carrying 20, so every decoder refused it as truncated and
nothing was ever asserted about tags. Nothing mechanical catches that — it would take a
second, permissive CBOR reader, and a check built on the strict decoder's first complaint
would depend on the order that decoder happens to look in.
