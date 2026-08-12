;; A block looking for a re-entrant call, and never finding one (ABI §1.2, §6.2, §13.2).
;;
;; ABI §1.2: "the host MUST NOT call into a guest that is mid-call. Guest→host calls MUST NOT
;; re-enter the guest." §6.2 is what makes that constructible rather than merely required:
;; **`emit` enqueues; it does not deliver.** Routing happens after the callback returns, so
;; there is no moment at which a delivery could arrive mid-callback.
;;
;; This block tries to catch one anyway. Every callback increments a depth counter on entry
;; and decrements it on exit; a callback that finds the counter already non-zero has been
;; re-entered, and sets a latch that no later call clears. It emits three times mid-callback
;; — the only guest→host call that could plausibly cause a delivery — and checks the depth
;; again after each.
;;
;; `eio_stop` returns the latch. A host that ever re-entered this block therefore fails its
;; scenario at the *stop* step even if nothing else looked wrong, which is the shape a probe
;; wants: the evidence outlives the moment.
;;
;; # What a passing run does and does not prove
;;
;; It proves this host did not re-enter *here*, under emissions the host had every
;; opportunity to route. It cannot prove no host ever will — nothing can, from inside a
;; guest. What makes re-entrancy unconstructible is the ABI's shape, and what makes it
;; checkable is that both host implementations run this file.
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  ;; `[{"n": 7}]`, the batch every emission carries. Its contents do not matter; that it is a
  ;; valid batch does, since a host is entitled to refuse a malformed one and refusing is not
  ;; routing.
  (data (i32.const 256) "\81\a1\61\6e\07")

  (global $next (mut i32) (i32.const 1024))
  (global $depth (mut i32) (i32.const 0))
  (global $reentered (mut i32) (i32.const 0))

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

  ;; Entering a callback. Anything but a depth of zero on the way in is a re-entry.
  (func $enter
    (if (i32.ne (global.get $depth) (i32.const 0))
      (then (global.set $reentered (i32.const 1))))
    (global.set $depth (i32.add (global.get $depth) (i32.const 1))))

  (func $leave
    (global.set $depth (i32.sub (global.get $depth) (i32.const 1))))

  ;; The probe: emit, then look. Three times, because a host that deferred routing by one
  ;; emission would still be caught by the next.
  (func $probe
    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (if (i32.ne (global.get $depth) (i32.const 1))
      (then (global.set $reentered (i32.const 1))))
    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (if (i32.ne (global.get $depth) (i32.const 1))
      (then (global.set $reentered (i32.const 1))))
    (drop (call $emit (i32.const 0) (i32.const 256) (i32.const 5)))
    (if (i32.ne (global.get $depth) (i32.const 1))
      (then (global.set $reentered (i32.const 1)))))

  (func (export "eio_configure") (param i32 i32) (result i32)
    (call $enter)
    (call $leave)
    (i32.const 0))

  ;; Emitting from `start` as well: ABI §5.1 permits it, and it is the one callback whose
  ;; emissions a host might be tempted to deliver before the instance is running.
  (func (export "eio_start") (result i32)
    (call $enter)
    (call $probe)
    (call $leave)
    (i32.const 0))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (call $enter)
    (call $probe)
    (call $leave)
    (i32.const 0))

  ;; The verdict, carried out of the run: `-1` if this block was ever inside two callbacks at
  ;; once, whichever callback it happened in.
  (func (export "eio_stop") (result i32)
    (if (i32.ne (global.get $reentered) (i32.const 0))
      (then (return (i32.const -1))))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"prober\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Emits mid-callback and watches for a re-entrant call\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
