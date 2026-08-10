;; A block that is valid in every way except the one this host enforces: it uses SIMD.
;;
;; ABI §4.3 places MVP conformance on the engine and nowhere else — `eio_manifest`
;; deliberately does no WASM feature gating — so this fixture is the evidence for that
;; sentence rather than an illustration of it. Every ABI §4.1 export is present with the
;; right signature, the manifest agrees with the module, and `eio_manifest::validate`
;; accepts it without complaint. The single `v128` in `eio_process_signals` is the only
;; thing wrong with it, and the engine is the only thing that can notice.
;;
;; What it protects: SIMD runs perfectly well under wasmtime at its defaults, so without the
;; MVP-only configuration this block would deploy on a daemon-class node and be refused by
;; wasm3 at flash time. That is the two-hosts divergence the shared crates exist to prevent
;; (DAEMON §1), arriving through the one door the shared crates do not watch.
(module
  (memory (export "memory") 1)

  (global $next (mut i32) (i32.const 1024))

  (func (export "eio_abi_version") (result i32)
    (i32.const 0x00010000))

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
    ;; The whole point of the fixture. Four lanes summed to nothing useful — what matters is
    ;; that `v128` appears at all.
    (i32x4.extract_lane 0 (v128.const i32x4 0 0 0 0)))

  (@custom "eio:manifest" "{\"name\":\"post_mvp\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"description\":\"Valid in every way except that it uses SIMD\",\"inputs\":[{\"name\":\"in\"}],\"outputs\":[]}")
)
