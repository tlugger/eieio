# Expression Language Specification

**Status:** Draft 1 **Depends on:** SCOPE.md §3.5 (decision record), ABI-SPEC.md §7.1 (evaluation protocol). This document specifies the language itself: grammar, data model, evaluation semantics, builtin library, and bounds. The protocol by which blocks obtain evaluated values is fixed in the ABI and not repeated here.

Working name: TBD (referred to below as "the expression language"; file extension for standalone snippets: `.sx`).

The key words MUST, MUST NOT, SHOULD, and MAY are used as in RFC 2119.

---

## 1. Design constraints (inherited, non-negotiable)

From SCOPE §3.5 and the ABI:

1. **Pure.** No IO, no clocks, no randomness, no host-function access. An expression is a function of (expression, signal, nothing else).
2. **Deterministic.** Same expression + same signal = same value, on every host, forever. This is the replay/debugging lever.
3. **Terminating by construction and by fuel.** No user-defined recursion, no unbounded loops; a host-enforced step budget backstops everything anyway.
4. **`no_std` implementable.** One small interpreter shared by daemon and leaf runtime. No feature may require an allocator-rich or OS-dependent construct.
5. **Per-signal evaluation.** The unit of evaluation is one expression against one signal (or against no signal — ABI `SIGNAL_NONE`).
6. **Agent- and human-writable.** Trivially parseable, trivially generatable, small enough to hold in one head or one context window.

---

## 2. Data model

The value space is exactly the CBOR subset fixed in ABI §6.3. Nothing exists in the language that cannot cross the boundary.

|Type|Notes|
|---|---|
|`null`||
|`bool`|`true`, `false`|
|`int`|Signed 64-bit. Arithmetic overflow is an **error**, not a wrap (determinism over convenience)|
|`float`|IEEE 754 binary64. `NaN` and infinities MUST NOT be produced: operations that would yield them (e.g. `(/ 1.0 0.0)`) are errors|
|`string`|UTF-8 text|
|`bytes`|Byte string. No literal syntax in v1; bytes enter only via signals|
|`array`|Ordered, heterogeneous|
|`map`|String keys only (matches signal shape). Iteration order: sorted by key (determinism)|

There is one additional evaluation-time-only type: **function** (§5.4). Functions are not values in the CBOR sense — an expression whose _final result_ is a function is an error (`ERR_EXPR`), and functions cannot be stored in arrays or maps.

---

## 3. Lexical structure and grammar

### 3.1 Tokens

```
expr    := atom | list
list    := "(" expr* ")"
atom    := number | string | symbol | sigil

number  := int | float
int     := ["-"] digit+
float   := ["-"] digit+ "." digit+ [exponent]
         | ["-"] digit+ exponent
exponent:= ("e"|"E") ["+"|"-"] digit+

string  := '"' char* '"'          ; escapes: \" \\ \n \t \r \u{XXXX}

symbol  := symstart symchar*
symstart:= letter | "+" | "-" | "*" | "/" | "=" | "<" | ">" | "!" | "?" | "_"
symchar := symstart | digit | "." | "-"

sigil   := "$" [symbol]           ; signal access, §6

comment := ";" .* end-of-line
```

Whitespace (space, tab, newline) separates tokens and is otherwise insignificant. `true`, `false`, and `null` are reserved symbols evaluating to themselves.

A parser MUST reject: unterminated strings/lists, integer literals outside i64, more than `MAX_DEPTH` (§9) nesting.

### 3.2 What is deliberately absent

No quote/quasiquote, no macros, no keywords, no chars, no rationals, no multiple namespaces, no reader dispatch. Every one of these is surface area the interpreter, the SDK docs, the Designer's expression editor, and every agent prompt would have to carry. Arrays and maps are constructed with functions (`arr`, `dict`), not literal syntax — one way to do it.

---

## 4. Evaluation model

Standard eager applicative-order evaluation with lexical scoping:

- A **number, string, `true`, `false`, `null`** evaluates to itself.
- A **symbol** evaluates to its binding (innermost `let`/`fn` scope, then the builtin table). Unbound symbol = `ERR_EXPR` at evaluation time (not parse time — but see §10 static checks).
- A **sigil** evaluates per §6.
- A **list** `(f a b ...)`: if `f` is a special form symbol (§5), apply special-form rules; otherwise evaluate `f` and all arguments left-to-right, then apply. Applying a non-function is an error.

### 4.1 Truthiness

Only `false` and `null` are falsy; every other value (including `0`, `""`, empty array/map) is truthy. Applies to `if`, `and`, `or`, `filter`, and the `?`-predicates' consumers uniformly.

### 4.2 Equality

`=` is deep structural equality. Numeric comparison is by mathematical value across int/float (`(= 1 1.0)` → `true`). `bytes` compare bytewise. Functions are never equal to anything.

---

## 5. Special forms

Exactly five. Everything else is a function.

### 5.1 `(if cond then else)`

Three arguments, always. `else` is mandatory — an expression must produce a value; silent-null branches are how config bugs hide. Evaluates exactly one branch.

### 5.2 `(let ((name expr) ...) body)`

Sequential (`let*`-style) binding: each binding sees earlier ones. Bindings are **not** recursive — a binding's expression cannot reference its own name. Shadowing builtins is permitted; shadowing `true`/`false`/`null` is a parse error.

### 5.3 `(and expr ...)` / `(or expr ...)`

Short-circuit. `and` returns the first falsy value or the last value; `or` returns the first truthy value or the last value. Zero arguments: `(and)` → `true`, `(or)` → `false`.

### 5.4 `(fn (param ...) body)`

Anonymous function, lexical closure over the enclosing environment. Fixed arity; arity mismatch at application is an error.

Functions exist so that `map`/`filter`/`reduce` exist (§7.5) — batch signals carry arrays, and per-element work without them forces logic back into custom blocks, which is the failure mode this language exists to prevent. Deliberate restrictions preserving termination:

- No `define`, no `letrec`, no self-reference: a function cannot name itself, so **recursion is unconstructible**.
- Functions are not CBOR values (§2): they cannot be a final result, stored in collections, or compared. They flow only as arguments to builtins.

Iteration exists only inside builtins over finite inputs; combined with no-recursion, every expression terminates. Fuel (§9) backstops pathological-but-finite cases.

---

## 6. Signal access

- `$` evaluates to the current signal (a map).
- `$name` is reader sugar for `(get $ "name")` — single-level, matching the overwhelmingly common case. Nested access is explicit: `(get-in $ (arr "a" "b"))`.
- **Missing attribute is an error**, not null: `$temp` on a signal without `"temp"` → `ERR_EXPR` for that signal (per ABI §7.1 per-signal failure semantics). Graceful handling is explicit: `(get-or $ "temp" 0)` or `(has? $ "temp")`. Silent null was rejected: it converts config typos into downstream mysteries.
- Under `SIGNAL_NONE` (ABI §7.1), evaluating `$` or any sigil → `ERR_NO_SIGNAL_CONTEXT`. Everything else evaluates normally.

`$` is the _only_ channel to signal data. There is no batch access, no neighboring-signal access, no index-of-current-signal: expressions are per-signal by construction, which is what makes host-side caching and constant folding (§10) sound.

---

## 7. Builtin library (v1)

Conventions: `ERR` marks argument-type or domain errors (all surface as `ERR_EXPR` through the ABI). All builtins are total over their documented domains and error outside them — no implicit coercion except int→float promotion in mixed arithmetic.

### 7.1 Arithmetic

|Form|Result|
|---|---|
|`(+ n ...)` `(- n ...)` `(* n ...)`|Variadic; int unless any float (promote); int overflow ERR. `(- n)` negates|
|`(/ a b)`|Float division always. `b` numerically zero → ERR|
|`(div a b)` `(mod a b)`|Integer floor-division/modulo; ints only; zero divisor ERR|
|`(min n ...)` `(max n ...)` `(abs n)`||
|`(floor f)` `(ceil f)` `(round f)`|→ int; ERR if result exceeds i64. Round: half away from zero|

### 7.2 Comparison and logic

|Form|Result|
|---|---|
|`(= a b)` `(!= a b)`|Deep equality (§4.2)|
|`(< a b)` `(<= a b)` `(> a b)` `(>= a b)`|Numbers, or two strings (lexicographic by Unicode scalar). Mixed → ERR|
|`(not x)`|Truthiness-based|

### 7.3 Type predicates and conversion

|Form|Result|
|---|---|
|`(null? x)` `(bool? x)` `(int? x)` `(float? x)` `(number? x)` `(string? x)` `(bytes? x)` `(array? x)` `(map? x)`||
|`(int x)`|From float (truncate), numeric string, bool. Else ERR|
|`(float x)`|From int, numeric string. Else ERR|
|`(string x)`|From any non-function value; canonical rendering (§7.6)|

### 7.4 Strings

|Form|Result|
|---|---|
|`(str x ...)`|Concatenation of canonical renderings|
|`(len s)`|Unicode scalar count (also arrays/maps/bytes: element/entry/byte count)|
|`(upper s)` `(lower s)` `(trim s)`|ASCII-only case mapping in v1 (`no_std` locale honesty)|
|`(contains? s sub)` `(starts-with? s p)` `(ends-with? s p)`||
|`(substr s start len)`|Scalar-indexed; out-of-range clamps, negative ERR|
|`(split s sep)` → array|`(join arr sep)` → string; non-string elements ERR|
|`(index-of s sub)`|Scalar index or `-1`|

### 7.5 Collections

|Form|Result|
|---|---|
|`(arr x ...)`|Array constructor|
|`(dict k v k v ...)`|Map constructor; keys MUST be strings; even arity|
|`(get c k)`|Map: missing key ERR. Array: int index, out-of-range ERR|
|`(get-or c k default)`|Non-erroring `get`|
|`(get-in c ks)`|Nested `get` along an array of keys/indices|
|`(has? c k)`|Membership (map key / array index validity)|
|`(first a)` `(last a)`|Empty → ERR|
|`(slice a start len)`|Clamping, like `substr`|
|`(concat a b ...)`|Arrays only|
|`(assoc m k v)`|Returns new map (persistent; inputs never mutate)|
|`(keys m)` `(vals m)`|Sorted-by-key order (§2)|
|`(range n)` `(range start end)`|Int array; length capped by `MAX_RANGE` (§9)|
|`(map f a)` `(filter f a)`|`f` unary|
|`(reduce f init a)`|`f` binary `(acc elem)`|
|`(any? f a)` `(all? f a)`|Short-circuit|
|`(sort a)`|Homogeneous numbers or strings; else ERR. Stable ascending|
|`(contains? a x)`|Also serves arrays (deep equality)|

### 7.6 Canonical rendering

`(string x)` / `(str ...)` output is normatively fixed (conformance vectors pin it): ints base-10; floats shortest-roundtrip; `true`/`false`/`null` as spelled; strings as-is (unquoted); bytes lowercase hex; arrays/maps in a JSON-like rendering with sorted map keys. Two hosts MUST render identically — rendered strings end up in signals and must not diverge across nodes.

### 7.7 Not in v1 (and why)

Regex (interpreter weight + `no_std` + ReDoS-vs-fuel headaches; revisit demand-driven), date/time parsing or formatting (impurity-adjacent, tz tables are enormous; timestamps are ints, blocks own formatting), string formatting mini-language (`str` composes), math beyond arithmetic (`sqrt`/`pow`/trig — additive later if sensor math demands it), bytes manipulation (additive later; likely alongside an `spi`/`uart` capability wave). All additive: minor version.

---

## 8. Errors

Every error carries a code, a source span (byte offsets into the expression text), and a message. Codes:

|Code|Condition|
|---|---|
|`PARSE`|Lexical/syntactic rejection, at property-load time|
|`UNBOUND`|Unknown symbol|
|`TYPE`|Wrong argument type / non-function application / function as final result|
|`ARITY`|Wrong argument count (special forms and `fn` application)|
|`DOMAIN`|Division by zero, int overflow, out-of-range, NaN-producing op|
|`NO_SIGNAL`|Sigil under `SIGNAL_NONE`|
|`MISSING`|`get` on absent key/index, `first` of empty|
|`FUEL` / `DEPTH` / `SIZE`|Budget exceeded (§9)|
|`RESULT_TYPE`|Final value fails the manifest-declared property type (ABI §11)|

Mapping to the ABI: `PARSE` errors surface at configure time (configuration rejection); `NO_SIGNAL` maps to `ERR_NO_SIGNAL_CONTEXT`; everything else maps to `ERR_EXPR`, per-signal, instance unaffected. Hosts MUST log code + span; the Designer and signal taps surface them (this is the 2-a.m.-debugging payoff of strict-over-null).

---

## 9. Bounds and determinism

Host-enforced budgets, checked during evaluation. Values are host configuration; normative **floors** (a conforming expression may rely on at least this much) and the reference defaults:

|Budget|Floor|Reference default|Notes|
|---|---|---|---|
|`MAX_FUEL` (eval steps)|10 000|100 000|One step ≈ one node visit / one builtin element touch|
|`MAX_DEPTH` (nesting + call depth)|32|128|Also enforced at parse|
|`MAX_RANGE`|1 000|65 536|`range` result length|
|`MAX_VALUE_BYTES`|4 096|262 144|Any constructed string/bytes/array/map, CBOR-encoded size|
|`MAX_EXPR_BYTES`|1 024|16 384|Source text length|

Exceeding a budget is a per-evaluation error (`FUEL`/`DEPTH`/`SIZE`), never a trap and never instance death — an expression cannot kill a block. Leaf hosts SHOULD sit near the floors.

Determinism restated as testable properties: no host function reachable, no clock, no RNG, map iteration sorted, float ops are IEEE 754 binary64 with no NaN/inf escape, canonical rendering pinned. The conformance vectors (§11) encode all of these.

---

## 10. Static analysis (normative minimum)

At configure time (ABI §7.1), the host MUST parse every property expression and:

1. Reject `PARSE` errors → configuration rejection.
2. Compute **signal dependence**: an expression is signal-dependent iff any sigil (`$`, `$name`) appears in it. This is the constant-folding predicate required by ABI §7.1 — signal-independent expressions are evaluated once and cached.
3. SHOULD reject statically-unbound symbols (every symbol resolvable to a binding form or builtin) at configure time rather than first evaluation. Catching typos at deploy, not at 2 a.m., is the point.

Hosts MAY additionally constant-fold sub-expressions, arity-check statically, or compile to bytecode — all invisible if conformance vectors pass.

---

## 11. Conformance

The monorepo carries `expr-tests/`: a host-agnostic vector suite (input expression, optional signal CBOR, expected value CBOR _or_ expected error code) covering every builtin, every special form, every error code, truthiness/equality tables, canonical-rendering pins, budget behavior at the floors, and signal-dependence classification. Both interpreter deployments (daemon, leaf) MUST pass identically. Divergence is a conformance bug by definition (same rule as ABI §13).

---

## 12. Examples (informative)

```lisp
; filter predicate: temperature above a threshold held in another attribute
(> $temp $threshold)

; derived attribute: severity bucket
(if (> $temp 90) "critical" (if (> $temp 75) "warn" "ok"))

; graceful default for an optional attribute
(get-or $ "unit" "C")

; per-signal computation over an embedded array
(let ((readings $samples))
  (/ (reduce (fn (acc r) (+ acc r)) 0.0 readings)
     (len readings)))

; string assembly for a topic-ish property
(str "sensor/" $device_id "/" (lower $kind))

; signal-independent (constant-folded once at configure)
(* 60 1000)
```

---

## 13. OPEN / deferred

- **Language name** — tracked with platform nomenclature, SCOPE §5.
- **Regex, math extensions, bytes ops, date/time** — deferred per §7.7; all additive (minor version). Expression-language versioning rides the ABI minor version: the builtin table and grammar for ABI 1.0 are exactly this document.
- **Designer affordances** (rendering literal-only expressions as plain input fields, expression linting UI) — UI concerns; no language surface.
