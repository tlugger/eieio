;; The receiving half of the end-to-end milestone: it frees what it is given, and reports a
;; per-signal property evaluated against the batch that was *routed* to it.
;;
;; Two things no other fixture does, both of them the milestone's point.
;;
;; **It obeys ABI §6.1.** "The guest owns the buffer and MUST `eio_free` it." The host never
;; frees an inbound payload — a host-side free would be a second owner (ABI §9.2) — so the
;; only thing that can prove the rule is a guest that counts. `$allocs` rises in `eio_alloc`,
;; `$frees` rises in `eio_free`, and `eio_stop` returns non-zero unless they match. Every
;; host→guest payload is covered, `eio_configure`'s included; `eio_start` and `eio_stop`
;; carry none and allocate nothing.
;;
;; The bump allocator still never reclaims, which stays legal — `eio_free` must be called,
;; not made to work. What is asserted here is the *discipline*, which is what a leaking block
;; would break.
;;
;; **It reads a property against a routed signal.** `doubled` is `(+ $n 41)`, which is
;; signal-dependent: it cannot be constant-folded at configure, and it fails outright on a
;; signal with no `n` (EXPR §6 — missing data is an error, not null). So a host that
;; evaluated it against the wrong signal, or against no signal, cannot produce 42.
;;
;; The value is emitted rather than checked in here, so the assertion lives in the test where
;; a failure is legible. The outbound batch is assembled at 256:
;;
;;   81            array(1)        the batch
;;   a1            map(1)          the signal
;;   63 "val"      "val": ...      3-byte key; 0x63 is also the letter `c`, hence "cval"
;;   ...           the property's own CBOR, written by `prop` straight into offset 262
;;
;; so the emitted signal is `{"val": 42}` whatever width the integer encodes to.
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))
  (import "eio:core" "prop" (func $prop (param i32 i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 256) "\81\a1cval")

  (global $next (mut i32) (i32.const 1024))
  ;; ABI §6.1's ledger. Equal at every quiescent moment; `eio_stop` is where that is checked.
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

  ;; The configure payload is a host→guest buffer like any other, so it is freed like one.
  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (func (export "eio_start") (result i32) (i32.const 0))

  ;; ABI §8: a non-zero status is reported and counted, and the instance stops regardless.
  ;; -1 is ERR_INVALID_ARG, which is the closest code to "this block leaked"; the test reads
  ;; the status rather than the code's name.
  (func (export "eio_stop") (result i32)
    (if (result i32) (i32.eq (global.get $allocs) (global.get $frees))
      (then (i32.const 0))
      (else (i32.const -1))))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $size i32)

    ;; ABI §8's size convention, as `probe.wat` uses it: ask with no buffer, then read into
    ;; one of the size named. `signal_idx` 0 is the routed signal — the batch this callback
    ;; was handed, not anything configure saw.
    (local.set $size
      (call $prop (i32.const 0) (i32.const 0) (i32.const 262) (i32.const 0)))
    (if (i32.le_s (local.get $size) (i32.const 0))
      ;; The property did not evaluate. Reported as the status rather than emitted, because
      ;; there is nothing to emit — and a silent zero-length emission would look like success.
      (then (return (i32.const -1))))
    (drop (call $prop (i32.const 0) (i32.const 0) (i32.const 262) (local.get $size)))

    (drop (call $emit (i32.const 0) (i32.const 256)
                (i32.add (i32.const 6) (local.get $size))))

    ;; Port 1 deliberately does not free, so that `eio_stop`'s check is provably able to
    ;; fire. A detector that has never fired is indistinguishable from one that cannot —
    ;; `canary.wat` reserves a port for the same reason.
    (if (i32.ne (local.get $port) (i32.const 1))
      (then (call $free (local.get $ptr) (local.get $len))))

    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"sink\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Frees what it is given and reports a per-signal property\",\"inputs\":[{\"name\":\"in\"},{\"name\":\"leak\"}],\"outputs\":[{\"name\":\"out\"}],\"properties\":[{\"name\":\"doubled\",\"type\":\"int\",\"default\":\"(+ $n 41)\"}]}")
)
