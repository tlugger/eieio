;; A block valid in every way except one: it uses extended const, which ABI §4.3 refuses.
;;
;; No `names` on the scenario, and this is the case ABI §4.3's SHOULD exists for: *no* engine
;; names this proposal. wasmtime answers "constant expression required: non-constant operator:
;; i32.add", which describes the instruction and never the feature; wasm3 answers "restricted
;; opcode". A vector asserting a name here would fail every conformant host.

;; The exports are stubs and nothing here ever runs: the assertion is that the module is
;; never loaded, so a working body would only be a body nobody reached. What it does have is
;; every ABI §4.1 export with the right signature and an agreeing `eio:manifest` section, so
;; `eio_manifest::validate` accepts it and the refusal can only be the engine's (ABI §13.1).
(module
  (memory (export "memory") 1)
  (global i32 (i32.add (i32.const 1) (i32.const 2)))
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"post-mvp\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
