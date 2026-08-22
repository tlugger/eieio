;; `state_del` on a key this instance never wrote (ABI §7.2, settled by eieio-7d8.16).
;;
;; `state_harness.wat` drives `state_get`/`state_put`'s undersized-buffer path because ABI
;; §13.2's golden counter never reaches it; this fixture exists for the same reason on
;; `state_del`, and for a case no golden block reaches at all — none of them ever calls
;; `state_del`. §7.2 now says a delete states the intended end state rather than a
;; transition: `0` whether or not `key` was present. That is the one thing a block cannot
;; observe from inside a real WASM module without deleting a key it deliberately never put,
;; which is exactly what this fixture's single call does — no `state_get` first, no seed, so
;; nothing but the host's own answer can make it come back `0`.
;;
;; The result is reported as the callback's own status, unmodified: a scenario reads one
;; number and knows exactly what `state_del` answered, with nothing else in the way that
;; could turn a wrong answer into a passing one.
;;
;;   0    "phantom"    a key this instance's state was never seeded with and never puts
(module
  (import "eio:state" "state_del" (func $state_del (param i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "phantom")

  (global $next (mut i32) (i32.const 1024))
  (global $allocs (mut i32) (i32.const 0))
  (global $frees (mut i32) (i32.const 0))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (global.set $allocs (i32.add (global.get $allocs) (i32.const 1)))
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32)
    (global.set $frees (i32.add (global.get $frees) (i32.const 1))))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (func (export "eio_start") (result i32) (i32.const 0))

  (func (export "eio_stop") (result i32)
    (if (result i32) (i32.eq (global.get $allocs) (global.get $frees))
      (then (i32.const 0))
      (else (i32.const -1))))

  ;; Deletes a key it never wrote and never asked about, and reports exactly what the host
  ;; answered — the scenario's assertion has nowhere else to live.
  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $status i32)

    (local.set $status
      (call $state_del (i32.const 0) (i32.const 7)))
    (call $free (local.get $ptr) (local.get $len))
    (local.get $status))

  (@custom "eio:manifest" "{\"name\":\"state-del-missing\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Deletes a key it never wrote and reports the status\",\"capabilities\":[\"state\"],\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
