;; Provokes each way the host refuses an `emit`, and reports what it was told.
;;
;; The input port selects the misbehaviour and the block returns `emit`'s answer as its own
;; callback status, so the host's code is observable without the block having to encode it.
;; None of these are traps: ABI §8's "status codes are life" is exactly what is being
;; checked, so a run that ends with the instance stopped normally is part of the assertion.
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

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

  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    ;; Port 1: the batch, shifted by a byte. Whatever that is, it is not the canonical CBOR
    ;; of ABI §6.3.1.
    (if (i32.eq (local.get $port) (i32.const 1))
      (then (return (call $emit
        (i32.const 0)
        (i32.add (local.get $ptr) (i32.const 1))
        (local.get $len)))))

    ;; Port 2: output port 9, of which this block has one.
    (if (i32.eq (local.get $port) (i32.const 2))
      (then (return (call $emit (i32.const 9) (local.get $ptr) (local.get $len)))))

    ;; Port 3: a length past any plausible `max_payload` (ABI §9.7). Refused on the length
    ;; alone, before the host reads a byte of it.
    (if (i32.eq (local.get $port) (i32.const 3))
      (then (return (call $emit (i32.const 0) (local.get $ptr) (i32.const 100000)))))

    ;; Port 4: the reserved error port (ABI §6.4), which every block has without declaring
    ;; it. Accepted like any other output — 0xFFFFFFFE is -2 as an i32 — and then routed, or
    ;; in a service that has not routed it, logged and counted.
    (if (i32.eq (local.get $port) (i32.const 4))
      (then (return (call $emit (i32.const -2) (local.get $ptr) (local.get $len)))))

    ;; Port 0: the well-formed case, so the others are a contrast rather than a baseline.
    (call $emit (i32.const 0) (local.get $ptr) (local.get $len)))

  (@custom "eio:manifest" "{\"name\":\"emitter\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"inputs\":[{\"name\":\"ok\"},{\"name\":\"malformed\"},{\"name\":\"badport\"},{\"name\":\"oversize\"},{\"name\":\"onerr\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
