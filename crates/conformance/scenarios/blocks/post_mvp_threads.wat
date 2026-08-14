;; A block valid in every way except one: it uses threads, which ABI §4.3 refuses.
;;
;; A shared memory is the threads proposal, and ABI §1.2 gives an instance one caller at a
;; time — so this is refused for the design invariant and not only for the feature flag.
;; wasm3 accepts and runs it (eieio-7d8.26), so the refusal is the loader's
;; (`eio_manifest::validate`), which is the same on every host (§4.3's layer 2).

;; The exports are stubs and nothing here ever runs: the assertion is that the module is
;; never loaded, so a working body would only be a body nobody reached. What it does have is
;; every ABI §4.1 export with the right signature and an agreeing `eio:manifest` section, so
;; the memory's `shared` flag is the only thing left for the refusal to be about (ABI §13.1).
(module
  (memory (export "memory") 1 1 shared)
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"post-mvp\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
