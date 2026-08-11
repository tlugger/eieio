;; `eio:state` with a four-byte first buffer, for the one state fault a golden block cannot
;; produce (ABI §7.2, §13.1).
;;
;; ABI §13.2's golden counter drives the rest of the state path — the round trip, a throttled
;; write, a denied capability — because it is a real `eio-sdk` block and those are answers any
;; block gets. **An undersized buffer is not.** The SDK starts a capability read from 64
;; bytes, and a counter that stores an integer never needs a second call, so `state_get`'s
;; grow-and-retry (ABI §8) is unreachable through it. This fixture offers four bytes, so a
;; scenario scripting a longer answer drives the retry and the second call appears in the
;; report — `10_state_grow_and_retry` is the only scenario left here.
;;
;; The contrast is the point rather than the coverage: the retry has to be visible when it
;; happens and absent when it does not, and `09_state_round_trip` is where it does not.
;;
;; Refusals come back as the callback's status, so a scenario reads one number and knows
;; which branch ran.
;;
;;   0    "count"                 the key
;;   256  81 a1 61 6e 07          `[{"n": 7}]`, emitted on success
;;   300  ..                      where a read lands
;;   320  18 2a                   `42`, what is written back
(module
  (import "eio:state" "state_get" (func $state_get (param i32 i32 i32 i32) (result i32)))
  (import "eio:state" "state_put" (func $state_put (param i32 i32 i32 i32) (result i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "count")
  (data (i32.const 256) "\81\a1\61\6e\07")
  (data (i32.const 320) "\18\2a")

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

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $size i32)
    (local $status i32)

    (local.set $status (call $count))
    (call $free (local.get $ptr) (local.get $len))
    (local.get $status))

  (func $count (result i32)
    (local $size i32)
    (local $put i32)

    ;; Four bytes, which is enough for a small integer and not for much else. Whether the
    ;; retry below runs is a property of the answer, not of this block.
    (local.set $size
      (call $state_get (i32.const 0) (i32.const 5) (i32.const 300) (i32.const 4)))
    (if (i32.lt_s (local.get $size) (i32.const 0))
      ;; `ERR_NOT_FOUND` on the first run, `ERR_CAPABILITY` when denied. Both are the host
      ;; telling this block something it can only report.
      (then (return (local.get $size))))
    (if (i32.gt_s (local.get $size) (i32.const 4))
      (then (drop (call $state_get (i32.const 0) (i32.const 5) (i32.const 300)
                        (local.get $size)))))

    (local.set $put
      (call $state_put (i32.const 0) (i32.const 5) (i32.const 320) (i32.const 2)))
    (if (i32.ne (local.get $put) (i32.const 0))
      ;; ABI §7.2: persistence is best-effort, so a refusal is reported and the instance
      ;; lives. A block treating this as fatal would be the defect §7.2 warns about.
      (then (return (local.get $put))))

    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"counter\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Reads and writes durable state, and reports every refusal as its status\",\"capabilities\":[\"state\"],\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
