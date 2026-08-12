;; The fixture for the assertions a *golden block* cannot make.
;;
;; Most scenarios drive ABI §13.2's golden blocks — real `eio-sdk` crates under
;; `examples/blocks/` — because driving what the platform actually produces is the point.
;; Two cannot, and this module is why they still exist:
;;
;;   `02_grow_and_retry`  asks for property 0 three times from a four-byte buffer, which is
;;                        how §7.1's "three calls, one evaluation" — the assertion that
;;                        catches a host that re-evaluates — is reachable at all. The SDK
;;                        retains its buffer and asks once.
;;   `06_emit_refusals`   checks §6.2's three fixed refusals from the guest's side. The SDK
;;                        refuses an oversized batch before the host sees it, and an
;;                        undeclared port is a compile error: making both unwritable is what
;;                        the SDK is for.
;;
;; It carries §13.1's guest-side allocation ledger for the same reason: a block written
;; through the SDK never sees `eio_alloc` or `eio_free`, so it has nothing of its own to
;; count. And it imports `eio:core` and nothing else, deliberately — a scenario is skipped
;; when its *module* declares a capability the host lacks, so a capability here would make
;; the daemon skip both of the above.
;;
;; # What each port is for
;;
;; `in` is the ordinary path. It reads property 0 three times and emits what it read:
;;
;;   1. `prop(.., cap = 4)`  — a first buffer, which the answer may or may not fit in.
;;   2. `prop(.., cap = n)`  — ABI §8's grow-and-retry, taken only when it did not.
;;   3. `prop(.., cap = n)`  — again, with nothing changed.
;;
;; The third call is the one worth having. ABI §7.1 requires the host to cache an evaluation
;; "for the duration of the current callback", so three calls MUST cost one evaluation — and a
;; host that re-evaluates passes every other assertion a scenario could make.
;;
;; `probe` is ABI §6.2's three fixed refusals, checked from the guest's side. They are the
;; ones §6.2 says are *not* host-defined, so a guest is entitled to the exact codes and this
;; block returns a distinct status naming whichever one was wrong.
;;
;; # The ledger
;;
;; `eio_stop` refuses unless every `eio_alloc` was matched by an `eio_free` (ABI §6.1). It has
;; to be the *guest* that checks: `eio_free` is an export, so a guest releasing an inbound
;; payload calls it internally and no host can see it (ABI §13.1). The bump allocator still
;; reclaims nothing, which stays legal — §6.1 requires the call, not the reuse.
;;
;; # The buffers
;;
;;   0    "count"       unused here; kept off the emitted region
;;   256  81 a1 63 76 61 6c    `[{"val": ` — the property's own CBOR lands at 262
;;   512  81 a1 61 6e 07       `[{"n": 7}]`, the fixed batch `probe` sends to the error port
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))
  (import "eio:core" "prop" (func $prop (param i32 i32 i32 i32) (result i32)))
  (import "eio:core" "log" (func $log (param i32 i32 i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "configured")
  ;; 0x63 is both the CBOR head for a 3-byte text string and the letter `c`, which is why the
  ;; key spells as "cval".
  (data (i32.const 256) "\81\a1cval")
  (data (i32.const 512) "\81\a1\61\6e\07")

  (global $next (mut i32) (i32.const 1024))
  (global $allocs (mut i32) (i32.const 0))
  (global $frees (mut i32) (i32.const 0))

  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))

  (func (export "eio_alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (global.set $allocs (i32.add (global.get $allocs) (i32.const 1)))
    (local.set $ptr (global.get $next))
    ;; Rounded up to eight, because ABI §9.6 makes an unaligned pointer the guest lying about
    ;; its own memory rather than merely being awkward.
    (global.set $next
      (i32.add
        (local.get $ptr)
        (i32.and (i32.add (local.get $size) (i32.const 8)) (i32.const -8))))
    (local.get $ptr))

  (func $free (export "eio_free") (param i32 i32)
    (global.set $frees (i32.add (global.get $frees) (i32.const 1))))

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $log (i32.const 2) (i32.const 0) (i32.const 10))
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (func (export "eio_start") (result i32) (i32.const 0))

  ;; ABI §8: a non-zero status is logged and counted, and the instance stops regardless.
  (func (export "eio_stop") (result i32)
    (if (result i32) (i32.eq (global.get $allocs) (global.get $frees))
      (then (i32.const 0))
      (else (i32.const -1))))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (local $status i32)

    (local.set $status
      (if (result i32) (i32.eq (local.get $port) (i32.const 1))
        (then (call $probe))
        (else (call $forward))))

    (call $free (local.get $ptr) (local.get $len))
    (local.get $status))

  ;; `in`: read the property and emit it.
  (func $forward (result i32)
    (local $size i32)
    ;; A real first buffer, not `cap = 0`: four bytes is enough for most integers and not
    ;; enough for all of them, so whether ABI §8's retry happens is a property of the value.
    (local.set $size
      (call $prop (i32.const 0) (i32.const 0) (i32.const 262) (i32.const 4)))
    (if (i32.lt_s (local.get $size) (i32.const 0))
      ;; The expression failed for this signal (ABI §7.1). Reported as the status rather than
      ;; emitted: there is nothing to emit, and a zero-length emission would look like success.
      (then (return (local.get $size))))
    (if (i32.gt_s (local.get $size) (i32.const 4))
      (then (drop (call $prop (i32.const 0) (i32.const 0) (i32.const 262) (local.get $size)))))
    ;; The same question again. One evaluation, or the host is not caching (ABI §7.1).
    (drop (call $prop (i32.const 0) (i32.const 0) (i32.const 262) (local.get $size)))

    (drop (call $emit (i32.const 0) (i32.const 256)
                (i32.add (i32.const 6) (local.get $size))))
    (i32.const 0))

  ;; `probe`: ABI §6.2's three fixed refusals, from the guest's side.
  (func $probe (result i32)
    ;; A `len` beyond `max_payload` is `ERR_LIMIT`, and ABI §13.2 requires the host to answer
    ;; it "without reading the payload". Stated as a pointer *outside linear memory*, which is
    ;; what makes the requirement checkable rather than merely written down: a host that
    ;; consulted the range before the length would fault on the read instead of returning
    ;; `ERR_LIMIT`, and this block would report `-1` instead of `0`.
    (if (i32.ne (call $emit (i32.const 0) (i32.const 100000) (i32.const 4096)) (i32.const -5))
      (then (return (i32.const -1))))
    ;; An output port that is not an index into the descriptor's `outputs` is
    ;; `ERR_INVALID_ARG`.
    (if (i32.ne (call $emit (i32.const 9) (i32.const 512) (i32.const 5)) (i32.const -1))
      (then (return (i32.const -2))))
    ;; `PORT_ERR` is reserved on every block and absent from the manifest (ABI §6.4), so this
    ;; one is accepted.
    (if (i32.ne (call $emit (i32.const 0xFFFFFFFE) (i32.const 512) (i32.const 5)) (i32.const 0))
      (then (return (i32.const -3))))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"harness\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Reads a per-signal property three times, and probes emit's three fixed refusals\",\"inputs\":[{\"name\":\"in\"},{\"name\":\"probe\"}],\"outputs\":[{\"name\":\"out\"}],\"properties\":[{\"name\":\"val\",\"type\":\"int\",\"description\":\"The signal's n, offset by 41\",\"default\":\"(+ $n 41)\"}]}")
)
