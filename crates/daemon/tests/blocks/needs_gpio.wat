;; A block whose manifest declares a capability this host does not implement.
;;
;; Load-time validation (ABI §4) passes: the import is a real `eio:gpio` function, the
;; manifest declares the capability, and the paired callback is present. What it cannot know
;; is whether *this node* provides GPIO, which is SCOPE §3.3's deploy-time question.
(module
  (import "eio:gpio" "gpio_read" (func $gpio_read (param i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  ;; No `eio_free` calls below, and none are owed: this module is refused at load, for a
  ;; capability the node does not implement (SCOPE §3.3), before any callback runs.
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (func (export "eio_on_gpio") (param i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"sensor\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[\"gpio\"]}")
)
