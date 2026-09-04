;; A block valid in every way, that declares two pages of linear memory (ABI §4.1).
;;
;; Nothing here is malformed and nothing is past §4.3's accepted set: §9.7 rule 10 lets a
;; block declare more than one page, and a host with room for it runs this module. What the
;; scenario asserts is the other half — that a host whose per-instance ceiling is one page
;; refuses it at load, rather than granting it less memory than it asked for or letting it
;; trap at whatever allocation first crossed the line.
;;
;; The exports are stubs and nothing here ever runs: the assertion is that the module is
;; never loaded. Every ABI §4.1 export is present with the right signature and the
;; `eio:manifest` section agrees, so the declared memory is the only thing left to object to.
(module
  (memory (export "memory") 2)
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"two-pages\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
