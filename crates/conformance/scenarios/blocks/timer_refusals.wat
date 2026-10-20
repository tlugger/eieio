;; `eio:timer`'s two refusals (ABI §7.3, settled by eieio-6c7.9).
;;
;; ABI §13.2's golden emitter arms one legal timer and cancels the id it was handed, so no
;; golden block reaches either refusal: a `delay_ms` that is not a delay, and a cancel naming
;; an id nothing armed. The hosts agreed on both before the spec said so — the reference
;; harness did not, and handed out an id for any delay — so this fixture is what holds every
;; host to the same answer.
;;
;; Each input port makes one call and reports what the host answered as the callback's own
;; status, unmodified, the way `state_del_missing.wat` does:
;;
;;   0  `zero`      timer_set(0, 0)      — ERR_INVALID_ARG: zero is not a delay
;;   1  `negative`  timer_set(-5, 0)     — ERR_INVALID_ARG
;;   2  `cancel`    timer_cancel(4242)   — ERR_NOT_FOUND: nothing armed that id
;;
;; It exports `eio_on_timer` because it imports `eio:timer` (ABI §4.2's pairing rule); nothing
;; here ever arms a timer, so the callback is never invoked.
(module
  (import "eio:timer" "timer_set" (func $timer_set (param i64 i32) (result i32)))
  (import "eio:timer" "timer_cancel" (func $timer_cancel (param i32) (result i32)))

  (memory (export "memory") 1)

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

  (func (export "eio_on_timer") (param i32) (result i32) (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $status i32)
    (local.set $status
      (if (result i32) (i32.eq (local.get $port) (i32.const 0))
        (then (call $timer_set (i64.const 0) (i32.const 0)))
        (else
          (if (result i32) (i32.eq (local.get $port) (i32.const 1))
            (then (call $timer_set (i64.const -5) (i32.const 0)))
            (else (call $timer_cancel (i32.const 4242)))))))
    (call $free (local.get $ptr) (local.get $len))
    (local.get $status))

  (@custom "eio:manifest" "{\"name\":\"timer-refusals\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Asks eio:timer for each of its refusals and reports the status\",\"capabilities\":[\"timer\"],\"inputs\":[{\"name\":\"zero\"},{\"name\":\"negative\"},{\"name\":\"cancel\"}],\"outputs\":[]}")
)
