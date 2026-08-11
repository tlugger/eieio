;; A timer-driven emitter: nothing is delivered to it, and it emits anyway (ABI §7.3, §6.2).
;;
;; The shape a simulator has, and the one that proves emission does not depend on delivery:
;; `eio_start` arms a timer and `eio_on_timer` emits. It also proves the pairing rule of ABI
;; §4.2 from the working side — this module imports `eio:timer` *and* exports `eio_on_timer`,
;; which is what load-time validation insists on in both directions.
;;
;; The id is whatever `timer_set` answered, kept in a global, and `eio_on_timer` refuses any
;; other. ABI §8 makes `0` a valid id, so a block that treated it as failure would arm nothing
;; and never notice.
(module
  (import "eio:timer" "timer_set" (func $timer_set (param i64 i32) (result i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 256) "\81\a1\61\6e\07")

  (global $next (mut i32) (i32.const 1024))
  (global $timer (mut i32) (i32.const -1))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  ;; ABI §5.1 step 3: a block MAY arm timers here, and this is the only place this one can —
  ;; nothing is ever delivered to it.
  (func (export "eio_start") (result i32)
    (global.set $timer (call $timer_set (i64.const 1000) (i32.const 1)))
    (if (result i32) (i32.lt_s (global.get $timer) (i32.const 0))
      (then (global.get $timer))
      (else (i32.const 0))))

  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_on_timer") (param $id i32) (result i32)
    (if (i32.ne (local.get $id) (global.get $timer))
      ;; A host firing an id this block never armed. `ERR_INVALID_ARG` is ABI §8's bad
      ;; parameter, and saying so is better than emitting on someone else's behalf.
      (then (return (i32.const -1))))
    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"emitter\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Arms a timer at start and emits when it fires\",\"capabilities\":[\"timer\"],\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
