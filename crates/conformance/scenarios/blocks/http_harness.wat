;; `eio:http`'s async `req_id` pattern and `eio_on_http`'s host-allocated inbound body
;; (ABI §4.2, §6.1, §7.6).
;;
;; `eio_on_http` is the only optional callback carrying an inbound payload — every other
;; capability that answers with data (`state_get`, `i2c_read`, `i2c_write_read`, `prop`) uses
;; the guest-supplied-buffer convention instead (ABI §9 rule 4). It is therefore the only place
;; besides `configure` and `process_signals` that exercises "host allocates, guest frees"
;; (§6.1, §9 rule 2), and `eio_free` is an export no engine surfaces to its embedder (§13.1) —
;; so this fixture is written to check it from the inside, the way `harness.wat` and
;; `state_harness.wat` check their own allocation ledgers.
;;
;; It requests once in `start` (the async pattern: the id a scenario fires the response against
;; is the one it scripted `http_request` to answer, not one this block tracks) and, on the
;; response, copies the exact bytes it was handed into an emitted batch and frees them. A host
;; that delivered an empty or truncated body, or that never freed, is visible: the former in the
;; emission, the latter in `eio_stop`'s balance check.
;;
;; The copy assumes an inbound body of 23 bytes or fewer, which keeps its CBOR byte-string head
;; a single byte (`0x40 | len`, ABI §6.3.1 rule 2's shortest head for a length in that range).
;;
;;   0    a0                 an opaque empty-map request payload; its content is not read
;;   300  81 a1 64 62 6f 64 79   `[{"body": ` — the head byte and body bytes follow at runtime
(module
  (import "eio:http" "http_request" (func $http_request (param i32 i32) (result i32)))
  (import "eio:core" "emit" (func $emit (param i32 i32 i32) (result i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "\a0")
  (data (i32.const 300) "\81\a1\64\62\6f\64\79")

  (global $next (mut i32) (i32.const 1024))
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

  (func (export "eio_configure") (param $ptr i32) (param $len i32) (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  ;; The async request-id pattern: fired here, answered later against whatever id the scenario
  ;; scripted `http_request` to hand back (ABI §7.6).
  (func (export "eio_start") (result i32)
    (local $req i32)
    (local.set $req (call $http_request (i32.const 0) (i32.const 1)))
    (if (i32.lt_s (local.get $req) (i32.const 0))
      (then (return (local.get $req))))
    (i32.const 0))

  (func (export "eio_stop") (result i32)
    (if (result i32) (i32.eq (global.get $allocs) (global.get $frees))
      (then (i32.const 0))
      (else (i32.const -1))))

  (func (export "eio_process_signals") (param $port i32) (param $ptr i32) (param $len i32)
        (result i32)
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  ;; `eio_on_http`: `(ptr, len)` is host-allocated and this block owns it from the moment the
  ;; call begins (ABI §6.1). It copies the exact bytes into the template above — so a scenario
  ;; asserting on the emission is asserting on the bytes the host actually delivered, not on
  ;; anything this block invented — and frees the payload before returning.
  (func (export "eio_on_http") (param $req i32) (param $status i32) (param $ptr i32)
        (param $len i32) (result i32)
    (i32.store8 (i32.const 307) (i32.or (i32.const 0x40) (local.get $len)))
    (memory.copy (i32.const 308) (local.get $ptr) (local.get $len))
    (drop (call $emit (i32.const 0) (i32.const 300) (i32.add (i32.const 8) (local.get $len))))
    (call $free (local.get $ptr) (local.get $len))
    (i32.const 0))

  (@custom "eio:manifest" "{\"name\":\"http_probe\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Requests once at start and echoes the host-allocated inbound body it is handed\",\"capabilities\":[\"http\"],\"inputs\":[],\"outputs\":[{\"name\":\"out\"}]}")
)
