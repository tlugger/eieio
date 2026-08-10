;; Missing `eio_stop`, so ABI §4.1's required-export check refuses it.
;;
;; Here to prove the daemon surfaces `eio_manifest`'s rejection reason rather than inventing
;; one of its own — the message a deployer reads has to name the export.
(module
  (memory (export "memory") 1)
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  ;; No `eio_free` calls below, and none are owed: this module is refused at load for the
  ;; export it lacks (ABI §4.1), so no callback runs and no payload is ever handed over.
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"broken\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0}}")
)
