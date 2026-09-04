;; A block that emits until the host's per-callback emission budget refuses it (ABI §9.7
;; rule 9, §6.2, §8).
;;
;; ABI §6.2 makes `emit` enqueue rather than deliver, so everything a callback emits is held
;; by the host until the callback returns. §9.7 rule 9 is the bound on what is held, and it is
;; the one limit in the descriptor that a *conforming* payload can hit: every emission below
;; is five bytes, far inside `max_payload`, and the third is refused only because the two
;; before it are still in the queue. A leaf publishes 4 096 and a daemon publishes nothing at
;; all, which is exactly why the rule is in the ABI rather than in one host's own spec.
;;
;; The block emits the same five-byte batch three times per callback against a budget of
;; twelve, and asserts the answers from the guest's side rather than leaving them to the
;; host's report: `0`, `0`, then `ERR_LIMIT` (-5). It names whichever answer was wrong by
;; returning -1, -2 or -3, so a status of 0 *is* the assertion — and a status of 0 is also
;; how the scenario checks that the refusal did not kill anything, since ABI §8 reserves
;; death for traps, fuel and deadlines and a refused emission is none of the three.
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  ;; `[{"n":1}]` — one signal, canonical CBOR, five bytes.
  (data (i32.const 256) "\81\a1\61\6e\01")

  (global $next (mut i32) (i32.const 1024))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))

  ;; Three emissions of five bytes each against a twelve-byte budget: the first two are
  ;; accepted and the third would make fifteen. The payload delivered is ignored — what this
  ;; block is about is the queue, not its input.
  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (if (i32.ne (call $emit (i32.const 0) (i32.const 256) (i32.const 5)) (i32.const 0))
      (then (return (i32.const -1))))
    (if (i32.ne (call $emit (i32.const 0) (i32.const 256) (i32.const 5)) (i32.const 0))
      (then (return (i32.const -2))))
    ;; ABI §8's `ERR_LIMIT`. A host that bounded nothing would answer 0 here, and a host that
    ;; killed the instance for it would never reach this comparison at all.
    (if (i32.ne (call $emit (i32.const 0) (i32.const 256) (i32.const 5)) (i32.const -5))
      (then (return (i32.const -3))))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"emission_budget\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Emits three fixed batches per callback against a budget that admits two\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
