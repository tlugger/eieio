;; An allocator that hands back memory it does not have (ABI §9 rule 6, §13.2's
;; allocator-liar).
;;
;; The third of the three answers `eio_alloc` can give wrongly, and the one `liar.wat` does
;; not cover: a pointer that is aligned, non-zero, and outside linear memory. ABI §9 rule 6
;; groups it with a misaligned pointer rather than with a refusal — "the guest has told the
;; host something untrue about its own memory, nothing the host does next is trustworthy,
;; and the instance MUST be discarded".
;;
;; Worth its own fixture because it fails at a *different place*. A misaligned pointer is
;; caught by the host's own check before anything is written (`liar.wat`); an out-of-bounds
;; one is caught by the write that follows, which is the engine refusing to touch memory
;; that is not there. Both must end the same way, and only running both shows that they do.
;;
;; It serves the descriptor first, for the reason `refuser.wat` gives: a block that lied
;; immediately would die in `configure`, which `liar.wat` already covers. This one dies on a
;; delivery, with a live instance behind it.
(module
  ;; One page. 100_000 is past the end of it and stays past the end: nothing here grows
  ;; memory, so the pointer cannot come true later.
  (memory (export "memory") 1)

  (global $next (mut i32) (i32.const 1024))
  (global $served (mut i32) (i32.const 0))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    ;; Aligned and non-zero, so it passes every check that can be made without asking the
    ;; engine — which is the point: this lie is only discoverable by trying to use it.
    (if (i32.ne (global.get $served) (i32.const 0))
      (then (return (i32.const 100000))))
    (global.set $served (i32.const 1))
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
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"oob\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Serves the descriptor and then answers eio_alloc outside linear memory\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
