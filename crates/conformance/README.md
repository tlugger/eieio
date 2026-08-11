# eio-conformance — the reference ABI harness

The harness of **ABI-SPEC §13.1**. It drives a module through ABI §5.1's whole lifecycle
under a scripted **scenario**, injects the faults §13.1 lists, and reports what a host did
against what the scenario said it must.

ABI §13 makes one claim the architecture rests on:

> Both the daemon and the leaf runtime MUST pass the harness against the golden blocks.
> Divergence between the two hosts is a conformance bug by definition.

This crate is what makes that checkable. It is **not** the host under test: `run` is generic
over a `Host`, and there are two implementations today —

|Host|Where|Implements|
|---|---|---|
|`Reference`|`src/reference.rs`|every ABI §7 namespace|
|the daemon|`crates/daemon/src/conformance.rs`|`eio:core` only (DAEMON §5.1)|

— running the same scenario files. Scenarios the daemon cannot reach are reported **skipped,
by name**, never counted as passes.

## Layout

```
scenarios/            the suite: one JSON document per scenario
scenarios/blocks/     the modules they drive, as reviewable .wat
src/scenario.rs       the format, as Rust types
src/run.rs            the walk over host-core's lifecycle driver
src/record.rs         the allocation ledger and the host-call log
src/core_fns.rs       a deterministic eio:core
src/capability.rs     scripted, deniable eio:state | timer | gpio | i2c | http
src/reference.rs      the reference wasmtime host
```

These are **not** `expr-tests/`. Those are at the repository root because a host written in
another language must consume them without building a Rust crate (EXPR §11, DAEMON §1); these
describe *host* behaviour, and every host is Rust for as far as anyone can see. The scenarios
are still data, for §13.1's reason: the leaf runtime must run the same ones.

## Writing a scenario

```json
{
  "name": "what-it-pins",
  "spec": "§7.1, §8",
  "module": "blocks/transform.wat",
  "limits": { "max_payload": 1024, "max_batch": 16 },
  "properties": { "val": "(+ $n 41)" },
  "steps": [
    { "action": "configure", "expect": { "status": 0 } },
    { "action": "start", "expect": { "status": 0 } },
    { "action": { "deliver": { "port": "in", "batch": "81a1616e01" } },
      "expect": { "status": 0, "calls": ["prop", "prop", "emit"], "evaluations": 1,
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
§6.1's "the guest MUST `eio_free` it" is therefore tested from the inside — the fixtures here
count their own allocations and refuse to stop unbalanced, as §13.2's golden blocks will — and
the harness's contribution is the leak signal: linear-memory growth across the run.

## Running it

```
cargo test --package eio-conformance      # the reference host
cargo test --package eio-daemon conformance   # the daemon's binding, same files
```

Both are part of `just ci`.
