;; A block that never returns from `eio_process_signals` (ABI §10, §13.2).
;;
;; Nothing here is malformed — it validates, configures and starts like any other block — so
;; the only thing that can end this callback is the host's execution budget. What a scenario
;; asserts is that the budget is what ends it, and which budget: fuel is deterministic work,
;; the deadline is wall-clock, and a host that conflated them would report a sizing problem as
;; a bug or the reverse.
;;
;; A bare `loop`/`br` and nothing else: no allocation, no host call, no memory traffic, so
;; there is nothing for the death to be attributed to but the budget.
(module
  (memory (export "memory") 1)

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

  ;; No `eio_free` calls anywhere below, and none are owed: this callback never returns, so
  ;; §6.1's "before the next callback at the latest" never arrives.
  (func (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (loop $forever (br $forever))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"spinner\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Never returns from process_signals\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
