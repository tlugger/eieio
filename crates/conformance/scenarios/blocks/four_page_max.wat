;; A block that declares one page and says it may grow to four (ABI §4.1).
;;
;; The other half of `two_pages.wat`. That module is refused for memory a host would have to
;; *supply*; this one is refused for memory a host would have to *honour* — its minimum fits
;; a one-page host exactly, and nothing about instantiating it would fail. What fails is
;; later: `memory.grow` is core WASM, the engine enforces the declared maximum and nothing
;; else, so an engine that instantiated this would let the guest reach 256 KiB on a host with
;; room for 64.
;;
;; The refusal is at the loader for the same reason `two_pages.wat`'s is: the ceiling is host
;; configuration, no engine has an opinion about it, and the alternative — instantiating and
;; then capping growth at one page — would grant the block less than it declared, which §4.1
;; refuses in as many words. A module that declared *no* maximum would say nothing to refuse
;; and is the engine's to bound, which is why there is no third fixture here.
;;
;; The exports are stubs and nothing here ever runs; every ABI §4.1 export is present with the
;; right signature and the `eio:manifest` section agrees, so the declared memory is the only
;; thing left to object to.
(module
  (memory (export "memory") 1 4)
  (func (export "eio_abi_version") (result i32) (i32.const 0x00010000))
  (func (export "eio_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "eio_free") (param i32 i32))
  (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
  (func (export "eio_start") (result i32) (i32.const 0))
  (func (export "eio_stop") (result i32) (i32.const 0))
  (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
  (@custom "eio:manifest" "{\"name\":\"four-page-max\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0},\"capabilities\":[],\"inputs\":[{\"name\":\"in\"}]}")
)
