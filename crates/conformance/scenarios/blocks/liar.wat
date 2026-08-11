;; An allocator that lies about its own memory (ABI §9.6, §13.2's allocator-liar).
;;
;; `eio_alloc` returns a pointer one byte past an aligned address. ABI §9.6 draws the line
;; this fixture sits on: a `0` is the guest *refusing*, which is true information honestly
;; reported and survivable (§9.5), while a misaligned pointer is the guest offering memory it
;; cannot honour — "nothing the host does next is trustworthy, so the instance MUST be
;; discarded".
;;
;; The very first inbound payload is the instance descriptor (ABI §5.1 step 2), so this block
;; dies during `eio_configure` and never runs a line of its own logic. That is the point: the
;; check has to be at the boundary, not half a callback later inside a guest doing a
;; misaligned load.
(module
  (memory (export "memory") 1)

  (global $next (mut i32) (i32.const 1024))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  ;; Aligned, then spoiled. Returning an unaligned *base* would be indistinguishable from an
  ;; allocator that simply never aligned anything; the `+1` says the block knows what eight
  ;; means and is answering with something else.
  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (i32.add (local.get $ptr) (i32.const 1)))

  (func (export "eio_free") (param i32 i32))

  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"liar\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Returns misaligned pointers from eio_alloc\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
