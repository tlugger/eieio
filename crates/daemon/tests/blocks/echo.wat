;; The smallest complete block: it takes a batch and emits it again, unchanged.
;;
;; Everything an ABI §4.1 module must have and nothing else, so that a test failing against
;; it is a failure of the *host* rather than of anything clever the guest did.
;;
;; The allocator is a bump pointer whose `eio_free` reclaims nothing, which is legal: §6.1
;; requires the call, not the reclamation. Not calling it would be a different thing
;; entirely, and this block used to conflate the two — "the guest owns the buffer and MUST
;; `eio_free` it" is the guest's obligation whatever its allocator then does with the
;; knowledge, because the host never frees an inbound payload (a host-side free would be a
;; second owner, ABI §9.2). `sink.wat` counts the pairs and fails `eio_stop` if they do not
;; match; this block just does its half.
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))
  (import "eio:core" "log" (func $log (param i32 i32 i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "configured")

  ;; The bump pointer. 1024 is past the data segment above and is a multiple of 8, and every
  ;; increment below is too, so ABI §9.6's alignment rule holds by construction.
  (global $next (mut i32) (i32.const 1024))

  (func (export "eio_abi_version") (result i32)
    ;; ABI §12: (major << 16) | minor, so 1.0.
    (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    ;; Round up to the next multiple of 8, and never hand out a zero-width block, so two
    ;; allocations can never share a pointer.
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $log (i32.const 2) (i32.const 0) (i32.const 10))
    ;; The configure payload is a host→guest buffer like any other (ABI §6.1).
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    ;; The inbound buffer is the guest's from the moment this call began (ABI §9.2), so
    ;; emitting out of it is emitting out of guest-owned memory — which is what ABI §6.2
    ;; asks for.
    (drop (call $emit (i32.const 0) (local.get $ptr) (local.get $len)))
    ;; Freed *after* the emit, not before: the host copies the payload out during the call
    ;; (ABI §9.3), so the bytes have to still be the guest's while `emit` reads them.
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"echo\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Emits every batch it receives\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}],\"properties\":[{\"name\":\"threshold\",\"type\":\"int\",\"default\":\"22\"},{\"name\":\"label\",\"type\":\"string\",\"required\":true},{\"name\":\"filter\",\"type\":\"string\"}]}")
)
