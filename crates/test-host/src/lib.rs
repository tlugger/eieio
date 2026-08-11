//! `TestHost` — run a block natively, with no WASM engine (SDK-SPEC §6.1).
//!
//! The fast inner loop a block author works in: build a block, hand it a batch, assert on
//! what it emitted.
//!
//! ```
//! use eio_sdk::{Block, Bound, Value};
//! use eio_test_host::{PropertyType, TestHost, batch, signal};
//!
//! # struct MyBlock;
//! # impl Block for MyBlock {}
//! # impl Bound for MyBlock { fn bound() -> MyBlock { MyBlock } }
//! let mut host = TestHost::<MyBlock>::builder()
//!     .inputs(["default"])
//!     .outputs(["above", "below"])
//!     .property("threshold", PropertyType::Float, "50.0")
//!     .start()
//!     .expect("it configures and starts");
//!
//! host.deliver("default", batch([signal([("value", Value::Float(70.0))])]))
//!     .expect("delivered");
//!
//! // `MyBlock` emits nothing, so nothing arrives — a real block's assertion goes here.
//! assert!(host.emitted("above").is_empty());
//! ```
//!
//! # Why this is a host, and lives outside the SDK
//!
//! It drives a block the way a daemon does, and it reuses the daemon's own machinery to do
//! it: **properties are resolved by `host-core`'s `PropContext`** — the real `expr`
//! interpreter, the real per-callback cache, the real constant folding, the real
//! declared-type check. Not an approximation of ABI §7.1 but the implementation of it, so
//! a property that fails here fails on a node for the same reason.
//!
//! That is also why it is not part of `eio-sdk`. `eio-sdk` compiles into guests, and a
//! guest reaching for `host-core` would drag the expression interpreter and the manifest
//! parser into every block — the coupling `eio-abi` was extracted to prevent. A mock host
//! is a host; its dependencies belong on the host side.
//!
//! # What it does not replace
//!
//! Everything here runs the block as **native Rust**. That is the point — it is fast, it
//! debugs, and it has no engine to boot — but it means the boundary itself is not under
//! test: no linear memory, no `(ptr, len)`, no CBOR crossing an engine, no fuel and no
//! deadline. Those are the conformance harness's (ABI §13, eieio-7d8.6), and SDK §6's two
//! layers exist because neither layer catches the other's bugs.

use std::cell::RefCell;
use std::rc::Rc;

use eio_host_core::{Arg, EngineError, HostCall, Memory, PropContext, PropFailure, PropertySource};
use eio_sdk::{Batch, Block, BlockError, Bound, Ctx, Descriptor, Limits, Value};
use eio_signal::Signal;

mod capabilities;

pub use capabilities::{Scripted, Throttle};

/// ABI §11.1's property types, re-exported.
///
/// Every `.property(..)` call names one, so a block's test crate would otherwise carry a
/// dependency on `eio-manifest` for a single enum — friction in the wrapper, which ABI §14
/// calls the wrapper's bug rather than the author's.
pub use eio_manifest::PropertyType;

/// What a block emitted on one port during one callback.
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    /// The port's name, resolved from the index the block emitted on.
    pub port: String,
    /// The batch, decoded from the canonical CBOR the block wrote.
    pub batch: Batch,
}

/// A block under test, before it has been configured.
///
/// Split from [`TestHost`] so the shape of the instance is fixed before anything runs:
/// ABI §5.2 makes port and property indices immutable for the life of an instance, and a
/// builder that let a test add a port after `start()` would be modelling something the ABI
/// does not permit.
#[derive(Debug)]
pub struct Builder<B> {
    block: B,
    inputs: Vec<String>,
    outputs: Vec<String>,
    properties: Vec<Property>,
    limits: Limits,
    instance_id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct Property {
    name: String,
    ty: PropertyType,
    source: Option<String>,
}

impl<B: Block> Builder<B> {
    /// The input ports, in index order (ABI §5.2).
    pub fn inputs<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.inputs = names.into_iter().map(Into::into).collect();
        self
    }

    /// The output ports, in index order (ABI §5.2).
    pub fn outputs<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.outputs = names.into_iter().map(Into::into).collect();
        self
    }

    /// A property and the expression configured for it, in `prop_id` order (ABI §5.2).
    ///
    /// `source` is an expression, not a value — ABI §11 admits no other kind of property,
    /// and a literal is a trivial expression. `"50.0"` and `"(float $value)"` are both
    /// ordinary here.
    pub fn property(
        mut self,
        name: impl Into<String>,
        ty: PropertyType,
        source: impl Into<String>,
    ) -> Self {
        self.properties.push(Property {
            name: name.into(),
            ty,
            source: Some(source.into()),
        });
        self
    }

    /// A property the deployment supplied **nothing** for (ABI §7.1, §11.1).
    ///
    /// Keeps its `prop_id` and answers `ERR_NOT_FOUND` for every `signal_idx`. Worth
    /// testing on purpose: §11.1 makes "not required, no default, nothing supplied" a
    /// valid declaration, and the block's own fallback is the branch it exercises.
    pub fn unset_property(mut self, name: impl Into<String>, ty: PropertyType) -> Self {
        self.properties.push(Property {
            name: name.into(),
            ty,
            source: None,
        });
        self
    }

    /// The limits the descriptor publishes (ABI §5.2, §9.7).
    ///
    /// Neither has a floor, so a test that wants to exercise a block against a small host
    /// sets them small — which is the only way to find out whether a block reads them.
    pub fn limits(mut self, max_payload: u64, max_batch: u64) -> Self {
        self.limits = Limits {
            max_payload,
            max_batch,
        };
        self
    }

    /// Scripts what the capability stubs answer, before the lifecycle runs.
    ///
    /// Needed as a *builder* step and not only on the host, because `configure` and
    /// `start` are the first callbacks and a block routinely uses a capability in them —
    /// arming a timer, reading its own state. There would be no moment to script those
    /// answers otherwise.
    pub fn scripted(self, script: impl FnOnce(&Scripted<'_>)) -> Self {
        script(&Scripted::new());
        self
    }

    /// The instance id the descriptor carries.
    pub fn instance_id(mut self, id: impl Into<String>) -> Self {
        self.instance_id = id.into();
        self
    }

    /// Runs `eio_configure` and stops there (ABI §5.1).
    ///
    /// For a test about configuration itself — a property that will not compile, a block
    /// that rejects its own settings. An `Err` here is what the deployer would see.
    pub fn configure(self) -> Result<TestHost<B>, BlockError> {
        let descriptor = Descriptor {
            instance_id: self.instance_id.clone(),
            block: self.name.clone(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            props: self.properties.iter().map(|p| p.name.clone()).collect(),
            limits: self.limits,
        };

        let sources: Vec<PropertySource<'_>> = self
            .properties
            .iter()
            .map(|property| match &property.source {
                Some(source) => PropertySource::new(&property.name, property.ty, source),
                // ABI §7.1's "no value at all": the property keeps its `prop_id`.
                None => PropertySource::unset(&property.name, property.ty),
            })
            .collect();

        let properties = PropContext::compile(&sources).map_err(|error| {
            BlockError::Config(format!(
                "a property expression did not compile: {}",
                error.first()
            ))
        })?;

        let mut host = TestHost {
            block: self.block,
            ctx: Ctx::new(self.limits),
            descriptor,
            properties,
            emitted: Vec::new(),
            errors: Vec::new(),
            _answerer: Answerer::install(),
        };
        host.drain();

        let descriptor = host.descriptor.clone();
        let result = host.callback(None, |block, ctx| block.configure(ctx, &descriptor));
        result.map(|()| host)
    }

    /// Runs `eio_configure` then `eio_start` (ABI §5.1) — the ordinary way in.
    pub fn start(self) -> Result<TestHost<B>, BlockError> {
        let mut host = self.configure()?;
        host.start()?;
        Ok(host)
    }
}

/// A configured block, and the host driving it.
#[derive(Debug)]
pub struct TestHost<B> {
    block: B,
    ctx: Ctx,
    descriptor: Descriptor,
    properties: PropContext,
    emitted: Vec<Emission>,
    errors: Vec<String>,
    /// Uninstalls the property answerer when the host is dropped, so one test's block
    /// cannot answer another's properties.
    _answerer: Answerer,
}

impl<B: Block> TestHost<B> {
    /// Starts building a host around a fresh `B`.
    ///
    /// A builder rather than a constructor, because ABI §5.2 fixes the shape of an
    /// instance — ports, properties, limits — before anything runs, and a host that let a
    /// test add a port afterwards would model something no node can do.
    ///
    /// The block is constructed by the `#[block]` macro through [`Bound`], not passed in:
    /// binding each `Prop<T>` to its `prop_id` is the macro's job, and a test writing
    /// `Prop::new(PropId::new(0))` would be re-deriving — and free to get wrong — the one
    /// mapping ABI §5.2 says the manifest fixes.
    pub fn builder() -> Builder<B>
    where
        B: Bound,
    {
        Self::around(B::bound())
    }

    /// The same, around a block the caller built.
    ///
    /// For a test that needs the block to start with state a `Default` would not give it.
    /// The `Prop<T>` fields still have to be bound correctly, which is why this is not the
    /// way in.
    pub fn around(block: B) -> Builder<B> {
        Builder {
            block,
            inputs: Vec::new(),
            outputs: Vec::new(),
            properties: Vec::new(),
            limits: Limits {
                max_payload: 1 << 20,
                max_batch: 1 << 16,
            },
            instance_id: String::from("test-instance"),
            name: String::from("block-under-test"),
        }
    }

    /// `eio_start` (ABI §5.1).
    pub fn start(&mut self) -> Result<(), BlockError> {
        self.callback(None, |block, ctx| block.start(ctx))
    }

    /// `eio_stop` (ABI §5.1).
    pub fn stop(&mut self) -> Result<(), BlockError> {
        self.callback(None, |block, ctx| block.stop(ctx))
    }

    /// Delivers a batch on the named input port (ABI §6.1).
    ///
    /// The port is named rather than indexed, because that is what a test knows and ABI
    /// §5.2's index is an implementation detail of the descriptor. An unknown name is a
    /// panic and not an error: it is a mistake in the test, not a condition a block could
    /// meet.
    pub fn deliver(&mut self, port: &str, batch: Batch) -> Result<(), BlockError> {
        // Through the descriptor, which already resolves a name to ABI §5.2's index and is
        // the same lookup the block itself does in `configure`.
        let index = self.descriptor.input(port).unwrap_or_else(|| {
            panic!(
                "no input port {port:?}; this block declares {:?}",
                self.descriptor.inputs
            )
        });

        // ABI §9.7's inbound half, which a host enforces before the guest is called: a
        // batch beyond the published limits is never delivered, and the block is not
        // involved in the refusal.
        if batch.len() as u64 > self.descriptor.limits.max_batch {
            return Err(BlockError::msg(format!(
                "the batch has {} signals, beyond this instance's max_batch of {} — a host \
                 would refuse it before the block saw it (ABI §9.7)",
                batch.len(),
                self.descriptor.limits.max_batch
            )));
        }

        let batch = Rc::new(batch);
        self.callback(Some(batch.clone()), move |block, ctx| {
            block.process_signals(ctx, index, (*batch).clone())
        })
    }

    /// Delivers one signal as a batch of one — the common shape in a test.
    pub fn deliver_one(&mut self, port: &str, signal: Signal) -> Result<(), BlockError> {
        self.deliver(port, Batch::from_vec(vec![signal]))
    }

    /// Fires a timer, as the host would (ABI §4.2, §7.3).
    ///
    /// The host drives this rather than the block asking for it: a timer that fired is a
    /// *callback*, and scripting it means calling it. Whether the block armed this id is
    /// deliberately not checked — a host fires what it was asked to fire, and a block that
    /// mishandles an unknown id should be able to fail that way in a test.
    pub fn fire_timer(&mut self, timer: u32) -> Result<(), BlockError> {
        self.callback(None, |block, ctx| block.on_timer(ctx, timer))
    }

    /// Fires a GPIO edge (ABI §4.2, §7.4).
    pub fn fire_gpio(&mut self, watch: u32, level: eio_sdk::PinLevel) -> Result<(), BlockError> {
        let value = level.as_i32();
        self.callback(None, |block, ctx| block.on_gpio(ctx, watch, value))
    }

    /// Completes an HTTP request (ABI §4.2, §7.6).
    ///
    /// `status` below zero is a transport error and at or above zero is the HTTP status,
    /// exactly as the ABI defines it — so a test can script a DNS failure and a 404
    /// separately, which is the distinction a block retries on.
    pub fn complete_http(
        &mut self,
        request: u32,
        status: i32,
        body: &[u8],
    ) -> Result<(), BlockError> {
        let body = body.to_vec();
        self.callback(None, move |block, ctx| {
            block.on_http(ctx, request, status, &body)
        })
    }

    /// Everything emitted on `port` since the host was built, in emission order.
    ///
    /// Named rather than indexed for the same reason as [`TestHost::deliver`]. `PORT_ERR`
    /// is reachable as `"err"`, the name a service file routes it by (ABI §6.4, §11.1).
    pub fn emitted(&self, port: &str) -> Vec<&Batch> {
        self.emitted
            .iter()
            .filter(|emission| emission.port == port)
            .map(|emission| &emission.batch)
            .collect()
    }

    /// Every signal emitted on `port`, flattened across batches.
    ///
    /// The assertion a test usually wants: a block that emits one batch of three and a
    /// block that emits three batches of one have routed the same signals, and which
    /// shape it chose is rarely the point.
    pub fn signals(&self, port: &str) -> Vec<Signal> {
        self.emitted(port)
            .into_iter()
            .flat_map(|batch| batch.iter().cloned())
            .collect()
    }

    /// Every emission, in order, across all ports.
    pub fn emissions(&self) -> &[Emission] {
        &self.emitted
    }

    /// The structured error details the block reported through `eio:core` `error`
    /// (ABI §7.0, §8).
    pub fn reported_errors(&self) -> &[String] {
        &self.errors
    }

    /// Property evaluations that failed, as ABI §7.1's per-signal failures.
    ///
    /// A host logs these and surfaces them in taps; a test asserts on them. This is how a
    /// test tells "the block skipped that signal deliberately" from "the expression was
    /// wrong" — the two look identical from the emissions alone.
    pub fn property_failures(&self) -> Vec<PropFailure> {
        self.properties.take_failures()
    }

    /// The descriptor the block was configured with (ABI §5.2).
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// The block, for asserting on its own state.
    pub fn block(&self) -> &B {
        &self.block
    }

    /// Scripts what the capability stubs answer next (SDK §6.1).
    ///
    /// Queued, so a script set between two deliveries applies to the second.
    pub fn capabilities(&mut self) -> Scripted<'_> {
        Scripted::new()
    }

    /// Runs one guest callback, with the property scope ABI §7.1 requires around it.
    ///
    /// Everything funnels through here, which is what makes the scope unforgettable: the
    /// cache lives for exactly one callback, and the emissions drained afterwards are that
    /// callback's. ABI §6.2's "emit enqueues, it does not deliver" is the same shape — the
    /// host collects after the block returns.
    fn callback(
        &mut self,
        signals: Option<Rc<Batch>>,
        call: impl FnOnce(&mut B, &mut Ctx) -> Result<(), BlockError>,
    ) -> Result<(), BlockError> {
        ANSWERER.with(|slot| *slot.borrow_mut() = Some(Answering::new(&self.properties)));
        let result = {
            let block = &mut self.block;
            let ctx = &mut self.ctx;
            self.properties.during(signals, || call(block, ctx))
        };
        // What the generated export does on the way out (`runtime::finish`): an `Err`
        // reaches the host as structured detail through `eio:core` `error` *before* the
        // non-zero return. A host driving a block sees both, so a host modelling one has
        // to produce both — otherwise `reported_errors()` would be empty for exactly the
        // callbacks an operator would be reading it for.
        if let Err(error) = &result {
            self.ctx.error(error);
        }
        self.drain();
        result
    }

    /// Collects what the callback left in the recording stub (ABI §6.2).
    fn drain(&mut self) {
        for call in eio_sdk::raw::take_calls() {
            match call {
                eio_sdk::raw::Call::Emit(port, bytes) => {
                    let name = if port as u32 == eio_host_core::PORT_ERR {
                        String::from(eio_manifest::PORT_ERR_NAME)
                    } else {
                        self.descriptor
                            .outputs
                            .get(port as usize)
                            .cloned()
                            .unwrap_or_else(|| format!("<undeclared port {port}>"))
                    };
                    let batch = Batch::from_cbor(&bytes)
                        .expect("a block emitted bytes that are not a canonical batch");
                    self.emitted.push(Emission { port: name, batch });
                }
                eio_sdk::raw::Call::Error(_, detail) => self.errors.push(detail),
                _ => {}
            }
        }
    }
}

thread_local! {
    /// What the installed answerer consults.
    ///
    /// A thread-local rather than a capture, because the answerer is installed once and
    /// each `TestHost` on this thread points it at its own context before every callback —
    /// which is what lets two hosts exist in one test without their properties crossing.
    static ANSWERER: RefCell<Option<Answering>> = const { RefCell::new(None) };
}

/// A `PropContext` and the `HostFn` over it, built once.
///
/// [`PropContext::host_fn`] allocates a fresh boxed closure and its own docs say to
/// register it once before the guest runs. Calling it per `prop` would put a heap
/// allocation on the per-signal path — a batch of ten thousand signals reading one
/// property would make ten thousand throwaway boxes, and a test host that is slow on big
/// batches is a test host people stop running.
struct Answering {
    call: Rc<RefCell<eio_host_core::HostFn>>,
}

impl Answering {
    fn new(context: &PropContext) -> Answering {
        Answering {
            call: Rc::new(RefCell::new(context.host_fn())),
        }
    }

    fn answer(&self, call: HostCall<'_>) -> eio_host_core::Ret {
        (self.call.borrow_mut())(call)
    }
}

/// Installs the process-wide property answerer, once per thread.
///
/// Once, and never removed — which is the second design here, because the first was wrong
/// in a way worth recording. It installed per host and restored the previous answerer on
/// drop, which is correct only if hosts are dropped in reverse order of construction.
/// `tests/two_hosts.rs` is the case that is not: build A, build B, drop A. A's "previous"
/// was `None`, so restoring it unhooked the answerer B was still using, and B's every
/// property came back `ERR_NOT_FOUND` — a failure that reads as a misconfigured block.
///
/// The fix is to notice the closure has no per-host state at all. It reads [`ANSWERER`],
/// which [`TestHost::callback`] points at the running host before every callback, so one
/// closure serves every host on the thread and there is nothing to take turns over. What
/// is left is idempotent installation.
#[derive(Debug)]
struct Answerer {
    _private: (),
}

thread_local! {
    /// Whether this thread's answerer is installed.
    static INSTALLED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

impl Answerer {
    fn install() -> Answerer {
        INSTALLED.with(|installed| {
            if installed.replace(true) {
                return;
            }
            eio_sdk::raw::set_prop_answerer(Some(Rc::new(|prop_id, signal_idx, buffer| {
                let answering = ANSWERER.with(|slot| {
                    slot.borrow().as_ref().map(|answering| Answering {
                        call: answering.call.clone(),
                    })
                });
                let Some(answering) = answering else {
                    // No host is driving, which means a block called `prop` outside a
                    // callback — ABI §7.1's answer for a `prop` with no scope.
                    return eio_host_core::ErrorCode::InvalidArg.as_i32();
                };
                // `host-core`'s own `prop`, reached the way an engine reaches it: the
                // buffer is this process's, exposed as guest memory at offset 0, so the
                // size convention and the grow-and-retry answer come out of the real
                // implementation rather than a retelling of it.
                let mut memory = SliceMemory { bytes: buffer };
                let call = HostCall {
                    args: &[
                        Arg::I32(prop_id),
                        Arg::I32(signal_idx),
                        Arg::I32(0),
                        Arg::I32(memory.bytes.len() as i32),
                    ],
                    memory: &mut memory,
                };
                match answering.answer(call) {
                    eio_host_core::Ret::I32(answer) => answer,
                    _ => eio_host_core::ErrorCode::InvalidArg.as_i32(),
                }
            })));
        });
        Answerer { _private: () }
    }
}

/// The block's own buffer, presented as guest memory at offset 0.
///
/// `host-core` writes a property's value through [`Memory`] because on a real host that is
/// linear memory. Here it is the slice the block passed to `prop`, so the value lands
/// exactly where the block is about to read it — with no copy and no second definition of
/// where "the buffer" is.
struct SliceMemory<'a> {
    bytes: &'a mut [u8],
}

impl Memory for SliceMemory<'_> {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        // `memory_range` and not a hand-written check: `host-core` exports it saying "a
        // third implementation would be the one to get it wrong", because it widens to
        // `u64` before adding so a near-`u32::MAX` pointer cannot wrap into a range that
        // looks valid. This would have been the third.
        let range = eio_host_core::memory_range(self.bytes.len(), ptr, len)?;
        Ok(self.bytes[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = eio_host_core::memory_range(self.bytes.len(), ptr, bytes.len() as u32)?;
        self.bytes[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// A signal from `(key, value)` pairs — the shape a test writes a fixture in.
pub fn signal<K: Into<String>>(fields: impl IntoIterator<Item = (K, Value)>) -> Signal {
    let mut signal = Signal::new();
    for (key, value) in fields {
        signal.set(key.into(), value);
    }
    signal
}

/// A batch from signals.
pub fn batch(signals: impl IntoIterator<Item = Signal>) -> Batch {
    Batch::from_vec(signals.into_iter().collect())
}
