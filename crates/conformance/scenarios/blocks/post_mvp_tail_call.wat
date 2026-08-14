;; A block valid in every way except one: it uses tail call, which ABI §4.3 refuses.
;;
;; wasm3 does not refuse this one — it loads, compiles and RUNS it, returning 1. So the
;; refusal is the loader's (`eio_manifest::validate`), which is the same on every host and
;; names the proposal on every host (§4.3's layer 2, eieio-7d8.26).

;; The exports are stubs and nothing here ever runs: the assertion is that the module is
;; never loaded, so a working body would only be a body nobody reached. What it does have is
;; every ABI §4.1 export with the right signature and an agreeing `eio:manifest` section, so
;; the `return_call` is the only thing left for the refusal to be about (ABI §13.1).
(module
  (memory (export "memory") 1)
  (func $g (result i32) (i32.const 1))
  (func (export "probe") (result i32) (return_call $g))
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"post-mvp\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
