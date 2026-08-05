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
|`float`|IEEE 754 binary64. `NaN` and infinities MUST NOT be produced: operations that would yield them (e.g. `(/ 1.0 0.0)`) are errors. They cannot *enter* via a signal either — ABI §6.3.1 rejects them at the decode boundary — so no value an expression ever sees is non-finite. Negative zero is a legal, distinct value|
|`string`|UTF-8 text|
|`bytes`|Byte string. No literal syntax in v1; bytes enter only via signals|
|`array`|Ordered, heterogeneous|
|`map`|String keys only (matches signal shape), unique. Iteration order: ascending bytewise order of the keys' UTF-8 content (determinism) — the same order the canonical encoding uses, ABI §6.3.1 rule 7|

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

string  := '"' char* '"'          ; escapes: \" \\ \n \t \r \u{HEX}
hex     := 1*6 hexdigit           ; a Unicode scalar value, §3.1.1

symbol  := symstart symchar*
symstart:= letter | "+" | "-" | "*" | "/" | "=" | "<" | ">" | "!" | "?" | "_"
symchar := symstart | digit | "." | "-"
letter  := "a".."z" | "A".."Z"    ; ASCII only, §3.1.1

sigil   := "$" [symbol]           ; signal access, §6

comment := ";" .* end-of-line
```

Whitespace (space, tab, newline, carriage return) separates tokens and is otherwise insignificant. `true`, `false`, and `null` are reserved symbols evaluating to themselves.

An expression source contains **exactly one** expression. Content after the first complete expression MUST be rejected: a property is one expression (ABI §11), so a second one could never be evaluated.

A parser MUST reject: unterminated strings/lists, integer literals outside i64, float literals denoting a non-finite value, malformed escapes, a number immediately followed by symbol characters, more than `MAX_DEPTH` (§9) nesting, and source longer than `MAX_EXPR_BYTES` (§9).

#### 3.1.1 Resolved details

Points the grammar above leaves open, fixed here so two implementations cannot diverge:

- **`letter` is ASCII alphabetic.** §7.4 already restricts case mapping to ASCII for `no_std` locale honesty, and Unicode identifier classification needs tables that do not fit the leaf-tier budget. A non-ASCII letter is therefore not a `symstart`.
- **`-` begins a number iff the next character is a digit**; otherwise it is a symbol. `-` is both a `symstart` and the number sign, so the grammar alone is ambiguous. Hence `-5` is a number while `-` and `-foo` are symbols, which is what lets `(- 1 2)` and `(- -1)` mean what they look like.
- **`\u{...}` takes one to six hex digits** (case-insensitive) and MUST name a Unicode scalar value. Surrogates (U+D800–U+DFFF) and anything above U+10FFFF MUST be rejected. The braces are what make the count variable; a fixed-width form would not need them.
- **A float literal denoting a non-finite value MUST be rejected** — `1e400`, for instance. §2 forbids operations from *producing* NaN or an infinity and ABI §6.3.1 rule 5 rejects one arriving in a signal; rejecting the literal closes the last route in, so no non-finite float can exist anywhere in the system.
- **A number MUST NOT run directly into symbol characters.** `1abc` is neither a number nor a symbol. Lexing it as `1` followed by `abc` would turn a typo into two valid tokens and surface the failure somewhere unrelated.
- **`1.` and `.5` are not numbers.** A float needs digits on both sides of the point, which is what keeps `.` unambiguous as a `symchar`.
- **Parse-time budget violations report `PARSE`**, not `SIZE` or `DEPTH`. §8 routes `PARSE` to configuration rejection and every other code to a per-signal `ERR_EXPR`. Source that is too long or nested too deeply is a property of the configuration, so it MUST reject the deployment rather than fail signals one at a time; `DEPTH` and `SIZE` are the evaluation-time codes.
- **The innermost unterminated list is the one reported**, since the most recently opened `(` is where the missing `)` belongs. Diagnostic only — which list a host names is not normative (cf. ABI §6.3.1).

### 3.2 What is deliberately absent

No quote/quasiquote, no macros, no keywords, no chars, no rationals, no multiple namespaces, no reader dispatch. Every one of these is surface area the interpreter, the SDK docs, the Designer's expression editor, and every agent prompt would have to carry. Arrays and maps are constructed with functions (`arr`, `dict`), not literal syntax — one way to do it.

---

## 4. Evaluation model

Standard eager applicative-order evaluation with lexical scoping:

- A **number, string, `true`, `false`, `null`** evaluates to itself.
- A **symbol** evaluates to its binding (innermost `let`/`fn` scope, then the builtin table). A symbol resolving to the builtin table yields a **function** (§5.4): `abs` on its own is a function value, which is what makes `(map abs $samples)` legal. Unbound symbol = `ERR_EXPR` at evaluation time (not parse time — but see §10 static checks).
- A **sigil** evaluates per §6.
- A **list** `(f a b ...)`: if `f` is a special form symbol (§5), apply special-form rules; otherwise evaluate `f` and all arguments left-to-right, then apply. Applying a non-function is a `TYPE` error, and so is the empty list `()`, which has nothing to apply (§10 rejects it statically, before any signal arrives).

### 4.1 Truthiness

Only `false` and `null` are falsy; every other value (including `0`, `""`, empty array/map) is truthy. Applies to `if`, `and`, `or`, `filter`, and the `?`-predicates' consumers uniformly. A function is truthy, being neither `false` nor `null`; none of §5.4's restrictions concerns truthiness.

### 4.2 Equality

`=` is deep structural equality. `bytes` compare bytewise.

Numeric comparison is by mathematical value across int/float (`(= 1 1.0)` → `true`), and it is **exact rather than by conversion**: `(= 9007199254740993 9007199254740992.0)` MUST be `false`, even though converting the int to a float makes the two indistinguishable. Ordering (§7.2) follows the same rule. An implementation that compares by converting one side agrees with this below 2⁵³ and then diverges silently, which is exactly the class of divergence §11 exists to catch.

**Comparing a function is a `TYPE` error**, in either operand position: `(= abs abs)`, `(= (fn (x) x) 1)` and `(!= abs 1)` all fail. A function is not a value (§2) and has no identity to compare; answering `false` instead would let a mistyped comparison evaluate quietly forever, which is the failure mode strict-over-null exists to prevent (§6).

---

## 5. Special forms

Exactly five. Everything else is a function.

### 5.1 `(if cond then else)`

Three arguments, always. `else` is mandatory — an expression must produce a value; silent-null branches are how config bugs hide. Evaluates exactly one branch.

### 5.2 `(let ((name expr) ...) body)`

Sequential (`let*`-style) binding: each binding sees earlier ones. Bindings are **not** recursive — a binding's expression cannot reference its own name. Rebinding a name already bound in the same binding list is ordinary `let*` and is permitted. Shadowing builtins is permitted; shadowing `true`/`false`/`null` is a parse error; shadowing one of the five special forms (§5) MUST be rejected by static analysis (§10). The last of those is not a style rule: §4 tests a list head against the special forms *before* resolving symbols, so a bound `if` would be inert in the one position that reads like a use of it.

### 5.3 `(and expr ...)` / `(or expr ...)`

Short-circuit. `and` returns the first falsy value or the last value; `or` returns the first truthy value or the last value. Zero arguments: `(and)` → `true`, `(or)` → `false`.

### 5.4 `(fn (param ...) body)`

Anonymous function, lexical closure over the enclosing environment. Fixed arity; arity mismatch at application is an error. Parameters bind simultaneously, so a repeated parameter name MUST be rejected — unlike a `let` binding list, where sequential scoping makes rebinding meaningful. A parameter MUST NOT shadow a special form, for the reason given in §5.2.

Functions exist so that `map`/`filter`/`reduce` exist (§7.5) — batch signals carry arrays, and per-element work without them forces logic back into custom blocks, which is the failure mode this language exists to prevent. Deliberate restrictions preserving termination:

- No `define`, no `letrec`, no self-reference: a function cannot name itself, so **recursion is unconstructible**.
- Functions are not CBOR values (§2): they cannot be a final result, stored in collections, or compared. They flow as operands — bound by `let`, passed to a builtin, applied directly — but never into a value.
- A **builtin is a function in this sense too** (§4): `abs` names one, and every restriction above applies to it identically. There is no second kind of callable.

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

Conventions: `ERR` marks argument-type or domain errors (all surface as `ERR_EXPR` through the ABI). All builtins are total over their documented domains and error outside them — no implicit coercion except int→float promotion in mixed arithmetic. Points the tables below leave open are resolved in §7.8.

**Arity.** Each named argument in a form is required, and a trailing `...` admits zero or more further arguments; a wrong count is `ARITY`. Six variadics are therefore total at zero arguments, where they answer the operation's identity: `(+)` → `0`, `(*)` → `1`, `(str)` → `""`, `(arr)` → `[]`, `(dict)` → `{}`, `(concat)` → `[]`. For `arr` and `dict` that is not a curiosity — it is the only way to write an empty array or map, since §3.2 gives neither a literal syntax. `-`, `/`, `min` and `max` have no identity and require their named arguments.

**Fold order.** Variadic arithmetic and `min`/`max` fold left to right, and an arithmetic accumulator promotes to float at the first float operand and stays float. Order is observable above 2⁵³, so it is fixed here rather than left to the implementation.

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

`(string x)` / `(str ...)` output is normatively fixed, down to the separators, and the conformance vectors pin it. Two hosts MUST render identically: rendered strings end up in signals and travel between nodes, so a rendering difference is a *data* difference.

- `null` renders `null`; `true` and `false` as spelled.
- **int**: base-10, a leading `-` when negative, no `+`, no leading zeros.
- **float**: the shortest decimal digit string that round-trips, placed as follows. Zero renders `0.0`, and negative zero `-0.0`. Otherwise, if the magnitude is at least `1e-4` and below `1e16`, fixed-point with **at least one digit after the point** (`1.0`, `0.5`, `0.0001`, `1000000000000000.0`); outside that range, scientific — one leading digit, the remaining digits after a point if there are any, then `e` and the exponent in base-10 with a `-` when negative and no `+` and no leading zeros (`1e16`, `9.999e-5`, `1.7976931348623157e308`, `5e-324`). The two bounds are what keep the rendering short and total: without an upper one `1e300` would be 301 characters, and without a lower one `5e-324` would be 324. With them, no float exceeds 24.
- **string**: its own characters, unquoted, at the top level; **quoted** when nested in an array or a map.
- **bytes**: lowercase hex, two digits per byte, no separator; unquoted at the top level and quoted when nested, like a string.
- **array**: `[`, then the elements in order separated by `, `, then `]`. Empty: `[]`.
- **map**: `{`, then the entries in ascending key order (§2) as `"key": value` separated by `, `, then `}`. Empty: `{}`. Keys are always quoted.
- **Quoting** uses exactly §3.1's escape set, so a rendered string re-reads as itself: `"` → `\"`, `\` → `\\`, U+000A → `\n`, U+0009 → `\t`, U+000D → `\r`, any other scalar below U+0020 → `\u{h…}` with lowercase hex and no leading zeros, and every other scalar as itself.
- A **function** has no rendering: `(string f)` and a function argument to `str` are `TYPE` (§2).

### 7.7 Not in v1 (and why)

Regex (interpreter weight + `no_std` + ReDoS-vs-fuel headaches; revisit demand-driven), date/time parsing or formatting (impurity-adjacent, tz tables are enormous; timestamps are ints, blocks own formatting), string formatting mini-language (`str` composes), math beyond arithmetic (`sqrt`/`pow`/trig — additive later if sensor math demands it), bytes manipulation (additive later; likely alongside an `spi`/`uart` capability wave). All additive: minor version.

### 7.8 Resolved details

Points §7.1–§7.5 leave open, fixed here so two implementations cannot diverge — the same role §3.1.1 plays for the grammar.

- **`min`/`max` return an argument unchanged**, without promotion: `(min 1 1.0)` is the int `1` and `(min 1.0 1)` is the float `1.0`. Comparison is by mathematical value (§4.2), so which of two numerically equal arguments comes back has to be said; it is the leftmost, so that the result does not depend on how equal values were spelled.
- **`div` and `mod` floor**, they do not truncate and they are not Euclidean. The three differ on negative operands: `-7 / 2` truncates to `-3` and floors to `-4`, and `(div -7 -2)` is `3` where Euclidean division gives `4`. `(mod a b)` therefore takes the sign of `b`.
- **`floor`, `ceil` and `round` accept an int** and return it unchanged. An int is already the answer; refusing it would make `(floor $count)` depend on whether the attribute arrived as an int or a float.
- **A "numeric string" is one matching §3.1's grammar** and nothing else: `(int s)` takes §3.1's `int`, and `(float s)` takes its `number` (so `(float "1")` works). No surrounding whitespace, no leading `+`, and none of the spellings a general-purpose float parser also accepts — `(int "inf")` and `(float "NaN")` MUST fail, or a non-finite float would be one step from a value (§2). `(int "1.5")` is `DOMAIN`: it asks for two conversions and names one, and `(int (float "1.5"))` says it.
- **`(float x)` does not accept a bool** though `(int x)` does. The asymmetry is §7.3's table as written: `(float (int b))` states that reading better than a second implicit rule would.
- **Type predicates are total over functions** and answer `false`: a function is not an int, and saying so is honest rather than silently wrong. So does `not`, which reads truthiness (§4.1) and finds a function truthy. Every *other* builtin refuses a function operand with `TYPE`, the five that take a function argument excepted.
- **`trim` removes exactly §3.1's whitespace** — space, tab, newline, carriage return. Not the Unicode `White_Space` property, whose table does not fit the leaf tier and which a host without it would apply differently.
- **`(split s "")` is `DOMAIN`.** An empty separator has no single obvious meaning, and `(map (fn (i) (substr s i 1)) (range (len s)))` states the character-wise reading explicitly.
- **`(join arr sep)` renders nothing**: a non-string element is `TYPE`, not canonically rendered. `(join (map string a) sep)` is the other reading, said out loud.
- **A duplicate key in `dict` is `DOMAIN`**, not last-wins. Keys there are written out one by one, so a repeat is a typo, and keeping one of the two values silently is how it reaches production.
- **`get-or` substitutes for absence only.** A key of a kind the container could never hold — a string against an array — is still `TYPE`: it is not absent, it was never a key that container has.
- **A negative array index is absent, not ill-typed**: `MISSING` from `get`, `false` from `has?`. It is an integer, which is the right kind of key for an array, and there is no from-the-end indexing in v1.
- **`range` is empty when its length would be ≤ 0**, rather than an error, so `(range (len a))` iterates nothing over an empty array. Longer than `MAX_RANGE` is `SIZE` (§9).
- **`sort`'s "homogeneous numbers" admits mixed int and float**; both are numbers and §4.2's exact ordering is total over them. Being stable, it keeps numerically equal elements — `1` and `1.0` — in their original order.
- **`(len x)`** counts Unicode scalars for a string, elements for an array, entries for a map, and bytes for a byte string; every other type is `TYPE`. String indices and lengths (`substr`, `index-of`) count scalars everywhere, never bytes.
- **`(get-in c ks)` with an empty path** is the container itself, which is what folding over no steps means.
- **`(any? f a)` and `(all? f a)` over an empty array** are `false` and `true` respectively — the vacuous readings.

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
|`MAX_DEPTH` (nesting + call depth)|32|128|Also enforced at parse, and on the nesting of a constructed value — see below|
|`MAX_RANGE`|1 000|65 536|`range` result length|
|`MAX_VALUE_BYTES`|4 096|262 144|Any constructed string/bytes/array/map, measured as the length of its **canonical** CBOR encoding (ABI §6.3.1 — exactly one encoding exists, so this length is unambiguous). Hosts SHOULD compute it structurally rather than by encoding, so that checking the budget does not cost the allocation the budget exists to prevent|
|`MAX_EXPR_BYTES`|1 024|16 384|Source text length|

Exceeding a budget is a per-evaluation error, never a trap and never instance death — an expression cannot kill a block. `MAX_FUEL` reports `FUEL`, `MAX_DEPTH` reports `DEPTH`, and `MAX_RANGE` and `MAX_VALUE_BYTES` report `SIZE`. Leaf hosts SHOULD sit near the floors.

Two of these budgets are checked while *reading* the text rather than while evaluating it — `MAX_EXPR_BYTES`, and the source-nesting half of `MAX_DEPTH` — and those report `PARSE` per §3.1.1, not the budget's own code. So `MAX_DEPTH` has two reporting sites: `PARSE` for source nesting, and `DEPTH` for call depth and for the nesting of a constructed value. The vectors of §11 pin both.

`MAX_DEPTH` bounds the nesting of a value an expression **builds**, not only the nesting of its source and its call stack. Without that, `(reduce (fn (acc x) (arr acc)) (arr) (range 65536))` is a fully budgeted expression whose result nests as deep as the range is long, and every walk over that value — *including dropping it* — then recurses that deep in the host. A stack overflow there kills the host, which ABI §8's "traps are death, status codes are life" does nothing to contain, and the 16 KiB-of-stack tier is the one that dies first. With the bound, every walk over a value in the system is provably shallow: a value arriving from a signal is bounded by the decode limit (ABI §6.3.1 rule 9, itself at least this `MAX_DEPTH`), and a value built by an expression is bounded here.

Determinism restated as testable properties: no host function reachable, no clock, no RNG, map iteration sorted, float ops are IEEE 754 binary64 with no NaN/inf escape, canonical rendering pinned. The conformance vectors (§11) encode all of these.

### 9.1 Step accounting

`MAX_FUEL` is not an exact cost model and MUST NOT be read as one. A host's step count is its own, bounded from both sides:

- **At least one step per node visited.** This is what makes fuel a termination backstop rather than a suggestion: evaluating any node costs something, so no loop inside a builtin and no chain of applications can run without the counter moving.
- **At most one step per node visited, one per function or builtin application, and one per element, map entry, or byte a builtin reads or produces.** This is what makes a floor a promise: an expression whose work fits within 10 000 of those units evaluates on every conforming host, whatever else that host might like to charge for.

Two consequences, both deliberate. A host MAY be cheaper than the ceiling — an O(*n* log *n*) `sort` charging *n* is conformant, because *n* is itself bounded by `MAX_VALUE_BYTES` and the work therefore stays finite. And **the step at which `FUEL` fires is not conformance-pinned.** Budgets are host configuration already: a leaf host near the floors and a daemon at the defaults disagree about that step by design, so pinning it would pin a number this section deliberately leaves free. What the vectors of §11 pin is the floor guarantee and the error *code*, not the step at which the code arrives.

Bytes rather than Unicode scalars for text, though every string operation in §7.4 is scalar-indexed. The two measure different things: §7.4 fixes what an expression *means*, and this fixes what it *costs*, where the UTF-8 length is both the honest measure of the work and the one a host does not have to walk the string to learn.

### 9.2 Configuring a budget

Every value in the table is host configuration. A host MAY set any of them above its floor, and a host that asks for one **below** its floor gets the floor — the value is clamped up rather than refused. A floor is a promise the language makes to expressions rather than advice a host may decline, and a deployment quietly running under a sub-floor budget would fail expressions in ways no vector could reproduce.

`MAX_DEPTH` is one budget enforced in three places — source nesting at parse time, combined nesting and call depth during evaluation, and the nesting of a constructed value — so a host configures it once and hands the same number to each.

---

## 10. Static analysis (normative minimum)

At configure time (ABI §7.1), the host MUST parse every property expression and:

1. Reject `PARSE` errors → configuration rejection.
2. Compute **signal dependence**: an expression is signal-dependent iff any sigil (`$`, `$name`) appears in it. This is the constant-folding predicate required by ABI §7.1 — signal-independent expressions are evaluated once and cached.
3. SHOULD reject statically-unbound symbols (every symbol resolvable to a binding form or builtin) at configure time rather than first evaluation. Catching typos at deploy, not at 2 a.m., is the point.

Item 3 obliges more than a symbol-table lookup. Resolving symbols requires knowing what each binding form binds, so a host implementing item 3 MUST also validate the *shape* of the five special forms (§5) in order to do it at all: what `(let (x) x)` binds has no answer, and a host that guessed would accept an expression that cannot evaluate. Such a host MUST reject:

- a `let` whose bindings are not a list of `(name expr)` pairs with a symbol in the name position, or which lacks exactly one body expression;
- a `fn` whose parameters are not a list of symbols, which repeats a parameter name, or which lacks exactly one body expression;
- an `if` with other than three arguments (§5.1);
- a binding or parameter that shadows a special form (§5.2, §5.4);
- a special form appearing anywhere but the head of a list, since §4 would then resolve it as an ordinary symbol and find nothing;
- an empty list `()`, which has nothing to apply (§4).

Diagnostics SHOULD be collected rather than reported one at a time: an expression editor (DESIGNER §5) shows every mistake at once, and a deploy that surfaces one typo per attempt wastes the operator's time. Which code a host attaches to each rejection is diagnostic rather than normative — hosts MUST agree on *whether* an expression is statically valid, not on how they describe a fault (cf. ABI §6.3.1).

Hosts MAY additionally constant-fold sub-expressions, arity-check builtin applications, or compile to bytecode — all invisible if conformance vectors pass.

---

## 11. Conformance

The monorepo carries `expr-tests/`: a host-agnostic vector suite covering every builtin, every special form, every error code, truthiness/equality tables, canonical-rendering pins, budget behavior at the floors, and signal-dependence classification. Both interpreter deployments (daemon, leaf) MUST pass identically. Divergence is a conformance bug by definition (same rule as ABI §13).

The vector file format is itself a contract, and **`expr-tests/README.md` is its normative description**: one JSON file per area of this specification, each vector an expression plus an optional signal, and either the value it must produce or the error code it must fail with. Values carry an explicit type tag, because `1` and `1.0` are different values (§4.2) and no untagged notation can say which one a vector means.

Two rules of this document shape the format and are worth restating where an implementer will meet them. A vector asserting a **static** rejection (§10) does not pin an error code — hosts MUST agree on whether an expression is rejected, not on how they describe the fault — and a vector asserting `FUEL` pins the code and the floor guarantee, never the step at which the code arrives (§9.1).

Coverage is not left to good intentions: the suite's runner enumerates the builtin table, the special forms, and this section's error codes, and fails if any of them has no vector. `RESULT_TYPE` is the sole exemption, being the host's property-type check (ABI §7.1) rather than an interpreter outcome.

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

- **Language name** — OPEN. Platform nomenclature is settled (SCOPE §5); this one is not. Referred to as "the expression language" until it is.
- **Regex, math extensions, bytes ops, date/time** — deferred per §7.7; all additive (minor version). Expression-language versioning rides the ABI minor version: the builtin table and grammar for ABI 1.0 are exactly this document.
- **Designer affordances** (rendering literal-only expressions as plain input fields, expression linting UI) — UI concerns; no language surface.
