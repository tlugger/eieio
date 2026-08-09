;; Exports an ABI version its manifest does not claim, and that this host does not implement.
;;
;; ABI §12 makes the *module* authoritative, so the manifest passing validation is not
;; enough: the exported version is what the host has to accept, and only running the guest
;; can read it.
(module
  (memory (export "memory") 1)
  (func (export "eio_abi_version") (result i32) (i32.const 0x00020000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"future\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0}}")
)
