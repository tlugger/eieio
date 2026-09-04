(module
  ;; `minimal.wat` with one thing changed: it declares one page and a maximum of four, so its
  ;; minimum fits a one-page host exactly and its declared growth does not (ABI §4.1). The
  ;; pair to `two_pages.wat`: that one is memory a host must supply, this is memory it must
  ;; honour. Conforming, and refused only by a host whose per-instance ceiling is below it.
  (memory (export "memory") 1 4)
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
