;; A block that emits more signals than the scenario's declared `max_batch` (ABI §5.2, §6.2,
;; §9.7).
;;
;; `max_batch` bounds the batches a host *delivers* to a guest; it does not bound emission.
;; §6.2's three refusals — non-canonical bytes, a bad `output_port`, a `len` beyond
;; `max_payload` — are the whole list, and a fourth (a batch beyond `max_batch`) is exactly
;; the guest-side check ABI §14 and this repo's history (eio-sdk once had one, and removed it)
;; say does not belong: a host is entitled to answer `ERR_LIMIT` on emission and none does, so
;; a guest refusing locally would report a code no host produces.
;;
;; This block does not read its input at all. On every delivery it emits one fixed batch of
;; four signals on `out`, regardless of what `max_batch` the descriptor published. The
;; scenario that drives it declares a `max_batch` smaller than four and asserts the emission
;; is accepted (status 0) and that all four signals arrive on the far side untouched — which a
;; host that silently bounded emission, or truncated it to `max_batch`, would fail.
(module
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  ;; `[{"n":1},{"n":2},{"n":3},{"n":4}]` — four signals, canonical CBOR.
  (data (i32.const 256) "\84\a1\61\6e\01\a1\61\6e\02\a1\61\6e\03\a1\61\6e\04")

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

  ;; Ignores the delivered payload entirely and emits the fixed four-signal batch. The
  ;; return value is `emit`'s own status, so a refusal here surfaces as this callback's
  ;; non-zero status rather than being swallowed.
  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (call $emit (i32.const 0) (i32.const 256) (i32.const 17)))

  (@custom "eio:manifest" "{\"name\":\"over_batch\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Emits a fixed batch of four signals, larger than any max_batch a scenario declares\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[{\"name\":\"out\"}]}")
)
