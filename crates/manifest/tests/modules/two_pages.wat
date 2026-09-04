(module
  ;; `minimal.wat` with one thing changed: it declares two pages of linear memory rather
  ;; than one. Conforming — ABI §9.7 rule 10 lets a block declare more than one page — and
  ;; therefore refused only by a host whose per-instance ceiling is below it (§4.1).
  (memory (export "memory") 2)
  (func (export "eio_abi_version") (result i32)
    i32.const 65536)
  (func (export "eio_alloc") (param i32) (result i32)
    i32.const 0)
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32)
    i32.const 0)
  (func (export "eio_start") (result i32)
    i32.const 0)
  (func (export "eio_stop") (result i32)
    i32.const 0)
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32)
    i32.const 0)
  (@custom "eio:manifest" "{\"name\":\"probe\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
