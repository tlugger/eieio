;; A block valid in every way except one: it uses GC, which ABI §4.3 refuses.
;;
;; Garbage-collected types. Refused twice over on a daemon: the cargo feature is compiled
;; out, so no `Config` call could admit it even if this configuration tried.

;; The exports are stubs and nothing here ever runs: the assertion is that the module is
;; never loaded, so a working body would only be a body nobody reached. What it does have is
;; every ABI §4.1 export with the right signature and an agreeing `eio:manifest` section, so
;; `eio_manifest::validate` accepts it and the refusal can only be the engine's (ABI §13.1).
(module
  (memory (export "memory") 1)
  (type $s (struct (field i32)))
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"post-mvp\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
