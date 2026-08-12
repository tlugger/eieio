;; An allocator that refuses, honestly (ABI §9 rule 5, §13.2's allocator-liar).
;;
;; The other half of `liar.wat`, and the half that must *not* be fatal. A `0` from
;; `eio_alloc` is the guest saying it could not allocate — true information about itself,
;; honestly reported — so ABI §9 rule 5 is explicit that "a host that cannot allocate an
;; inbound payload because the guest refused MUST NOT kill the instance. The delivery fails
;; and is reported as `ERR_LIMIT`". A transient memory spike is not a death sentence.
;;
;; # Why it has to serve the first allocation
;;
;; The instance descriptor is an inbound payload too (ABI §5.1 step 2), so a block that
;; refused everything would die in `configure` and never reach the case this fixture exists
;; for. It serves the descriptor and refuses afterwards, which is also the honest shape of
;; the thing being modelled: a guest whose heap filled up while it was running.
;;
;; A refusal is invisible from inside the guest — the host never calls `eio_process_signals`
;; at all — so everything asserted about this block is asserted from the host's side: the
;; status, the allocation ledger, and the fact that `eio_stop` still runs.
(module
  (memory (export "memory") 1)

  ;; Bumped past the descriptor's allocation. Nothing else is ever handed out.
  (global $next (mut i32) (i32.const 1024))
  (global $served (mut i32) (i32.const 0))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    ;; The descriptor, and then nothing. `0` is a legal answer at any point (§9 rule 5); the
    ;; count is only what lets this block get far enough to be asked twice.
    (if (i32.ne (global.get $served) (i32.const 0))
      (then (return (i32.const 0))))
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

  ;; Never reached: the host cannot build the payload to call it with. That is the assertion.
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"refuser\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Serves the descriptor and then answers eio_alloc with 0\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
