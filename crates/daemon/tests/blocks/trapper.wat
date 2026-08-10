;; A block that traps when it is given a batch.
;;
;; The other half of ABI §8's "traps are death, status codes are life": `probe.wat` returns a
;; non-zero status and lives, this one executes `unreachable` and does not. Configure and
;; start succeed, so the death is unambiguously the delivery's.
(module
  (memory (export "memory") 1)

  (global $next (mut i32) (i32.const 1024))

  (func (export "eio_abi_version") (result i32)
    (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  ;; ABI §6.1's free is deliberately absent: this callback traps, and a trap is instance
  ;; death (§8). The store goes with it, so there is no leak to observe and nothing that
  ;; freeing first would prove.
  (func (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (unreachable))

  (@custom "eio:manifest" "{\"name\":\"trapper\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Traps on delivery\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
