# eio-conformance — the reference ABI harness

The harness of **ABI-SPEC §13.1**. It drives a module through ABI §5.1's whole lifecycle
under a scripted **scenario**, injects the faults §13.1 lists, and reports what a host did
against what the scenario said it must.

ABI §13 makes one claim the architecture rests on:

> Both the daemon and the leaf runtime MUST pass the harness against the golden blocks.
> Divergence between the two hosts is a conformance bug by definition.

This crate is what makes that checkable. It is **not** the host under test: `run` is generic
over a `Host`, and there are three implementations today —

|Host|Where|Implements|
|---|---|---|
|`Reference`|`src/reference.rs`|every ABI §7 namespace|
|wasm3|`tests/wasm3.rs`|every ABI §7 namespace, on a second *engine*|
|the daemon|`crates/daemon/src/conformance.rs`|`eio:core` only (DAEMON §5.1)|

— running the same scenario files. Scenarios the daemon cannot reach are reported **skipped,
by name**, never counted as passes.

## Layout

```
scenarios/            the suite: one JSON document per scenario
scenarios/blocks/     the hand-written fixtures, as reviewable .wat
src/golden.rs         building ABI §13.2's golden blocks from examples/blocks/
src/scenario.rs       the format, as Rust types
src/run.rs            the walk over host-core's lifecycle driver
src/record.rs         the allocation ledger and the host-call log
src/core_fns.rs       a deterministic eio:core
src/capability.rs     scripted, deniable eio:state | timer | gpio | i2c | http
src/reference.rs      the reference wasmtime host
```

### Two kinds of module, and why both

Most scenarios drive a **golden block** (ABI §13.2): a real `eio-sdk` crate in
`examples/blocks/`, built by the ordinary toolchain with no flags. Driving what the platform
actually produces is the point — a host that can run a hand-written fixture and not a real
block is a host nobody can deploy to.

Three scenarios cannot, and keep a `.wat` fixture:

|Fixture|Scenario|Why a golden block cannot do it|
|---|---|---|
|`harness.wat`|`02_grow_and_retry`|Asks for property 0 three times from a four-byte buffer. The SDK retains its buffer and asks once, so §7.1's "three calls, one evaluation" — the assertion that catches a host that re-evaluates — would have nothing to assert|
|`harness.wat`|`06_emit_refusals`|§6.2's three fixed refusals. `Ctx::emit` refuses an oversized batch before the host sees it, and an undeclared port is a compile error: the SDK exists to make both unwritable|
|`state_harness.wat`|`10_state_grow_and_retry`|Same buffer question on the state path. A capability read starts from 64 bytes, and a counter storing an int never needs a second call|

`harness.wat` is also where §13.1's guest-side allocation ledger lives — a block written
through the SDK never sees `eio_alloc` or `eio_free`, so it has nothing to count. It imports
`eio:core` and nothing else, deliberately: a scenario is skipped when its *module* declares a
capability the host lacks, so a capability here would make the daemon skip these two.

`spinner.wat` and `liar.wat` are §13.2's hostile blocks, and stay hand-written for the reason
the whole table gives.

These are **not** `expr-tests/`. Those are at the repository root because a host written in
another language must consume them without building a Rust crate (EXPR §11, DAEMON §1); these
describe *host* behaviour, and every host is Rust for as far as anyone can see. The scenarios
are still data, for §13.1's reason: the leaf runtime must run the same ones.

## Writing a scenario

```json
{
  "name": "what-it-pins",
  "spec": "§7.1, §8",
  "module": "../../../examples/blocks/target/wasm32-unknown-unknown/release/transform.wasm",
  "limits": { "max_payload": 1024, "max_batch": 16 },
  "properties": { "val": "(+ $n 41)" },
  "steps": [
    { "action": "configure", "expect": { "status": 0 } },
    { "action": "start", "expect": { "status": 0 } },
    { "action": { "deliver": { "port": "in", "batch": "81a1616e01" } },
      "expect": { "status": 0, "calls": ["prop", "emit"], "evaluations": 1,
                  "emissions": [ { "port": "out", "batch": "81a16376616c182a" } ] } },
    { "action": "stop", "expect": { "status": 0 } }
  ],
  "expect": { "errors": 0 }
}
```

Three things about that document are worth knowing before writing another.

**Batches are hex, not JSON.** ABI §6.3.1 admits exactly one encoding of any batch, and
pinning bytes is half of what this suite is for. A JSON spelling would be a second, lossier
data model: no byte strings, and duplicate keys collapse before rule 7 can reject them.

**Ports and `prop_id`s are not in the document.** They come from the module's manifest,
resolved by ABI §11.1's `required`/`default` rule. A scenario supplies what a *service* would:
an instance id, the limits, and property expressions by name.

**`calls` and `evaluations` are different questions.** ABI §7.1 requires a property's value to
be cached for the callback's duration, so three `prop` calls MUST cost one evaluation. A single
number could not tell a compliant host from one that re-evaluates.

Every struct is `deny_unknown_fields`. A misspelt expectation does not fail — it silently
checks nothing, and the scenario passes forever.

## What the allocation ledger cannot see

Every run records the host's inbound `eio_alloc`s and checks two things a host can break on
its own: it must not call `eio_free` (ABI §9.2), and it must not write into memory it did not
allocate (§9.1).

It cannot see the *guest's* frees. `eio_free` is an export, so a guest releasing an inbound
payload calls it internally, and no engine surfaces an intra-module call to its embedder. ABI
§6.1's "the guest MUST `eio_free` it" is therefore tested from the inside — `harness.wat`
counts its own allocations and refuses to stop unbalanced — and the harness's contribution is
the leak signal: linear-memory growth across the run. A golden block cannot take that job:
every allocation and free in one is the SDK's audited glue (SDK §4), so it has nothing of its
own to count.

## Running it

```
cargo test --package eio-conformance      # the reference host, and wasm3
cargo test --package eio-daemon conformance   # the daemon's binding, same files
```

Each builds the golden blocks first, through `suite::run_own` — they are crates, not bytes
checked in, so there is nothing to drive until cargo has run.

Both are part of `just ci`.
