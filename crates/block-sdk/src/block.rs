//! The [`Block`] trait and [`Prop<T>`] — what a block author actually writes against
//! (SDK §1, §2).

use core::marker::PhantomData;

use eio_signal::{Batch, Value};

use crate::ctx::{Ctx, Descriptor, PropId, SignalIdx};
use crate::error::{BlockError, BlockResult};

/// What a block does (SDK §1).
///
/// **Every method has a default**, and that is deliberate rather than lenient. ABI §4.1
/// makes all eight exports REQUIRED, so the module carries them whatever the block
/// implements; what varies is whether there is anything to run behind one. A pure
/// transform has no `start`, and a timer-driven simulator has no `process_signals` at all
/// — ABI §6.2 explicitly admits blocks that "emit with no inbound batch". Requiring either
/// would make the honest shape of one of them a stub.
///
/// The callbacks are **not** `async`. No runtime exists in an instance and the ABI is
/// callback-shaped (SDK §3); long work is chunked through timers, and ABI §10 states the
/// contract plainly — callbacks MUST return promptly, and blocking is a defect.
#[allow(unused_variables)]
pub trait Block: Sized {
    /// Validate configuration and allocate internal state (ABI §5.1).
    ///
    /// Runs once, before anything else. Properties are readable here under
    /// [`SignalIdx::None`]; a signal-dependent one answers `ERR_NO_SIGNAL_CONTEXT`, which
    /// is how a block learns at configure time that an expression it needs per-signal was
    /// supplied where it cannot work.
    ///
    /// An `Err` **rejects the instance**: ABI §5.1 discards it and surfaces the error to
    /// the deployer. That is the right place to fail, and the only one where failing is
    /// free.
    fn configure(&mut self, ctx: &mut Ctx, descriptor: &Descriptor) -> BlockResult {
        Ok(())
    }

    /// Arm timers, register watches, emit initial signals (ABI §5.1).
    ///
    /// After this returns zero the host begins delivering batches.
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        Ok(())
    }

    /// Handle a batch on an input port (ABI §6.1).
    ///
    /// `input` is the port index — ABI §5.2 fixes it as the position in the descriptor's
    /// `inputs`, which is what the generated `In` enum names.
    fn process_signals(&mut self, ctx: &mut Ctx, input: u32, batch: Batch) -> BlockResult {
        Ok(())
    }

    /// Flush state and release resources (ABI §5.1).
    ///
    /// The host cancels outstanding timers, watches and requests *after* this returns. A
    /// stopped instance is never restarted.
    fn stop(&mut self, ctx: &mut Ctx) -> BlockResult {
        Ok(())
    }

    /// A timer fired (ABI §4.2). Requires `capabilities(timer)`.
    fn on_timer(&mut self, ctx: &mut Ctx, timer: u32) -> BlockResult {
        Ok(())
    }

    /// A watched GPIO edge fired (ABI §4.2). Requires `capabilities(gpio)`.
    fn on_gpio(&mut self, ctx: &mut Ctx, watch: u32, value: i32) -> BlockResult {
        Ok(())
    }

    /// An HTTP request completed (ABI §4.2, §7.6). Requires `capabilities(http)`.
    ///
    /// `status` below zero is a transport error; at or above zero it is the HTTP status.
    /// `body` is the CBOR `{headers, body}` map the host allocated — already copied out
    /// and freed by the generated export, so a block sees bytes it does not own.
    fn on_http(&mut self, ctx: &mut Ctx, request: u32, status: i32, body: &[u8]) -> BlockResult {
        Ok(())
    }
}

/// A block the `#[block]` macro can construct with its properties bound (SDK §1).
///
/// Implemented by the macro, never by hand. It exists because binding a `Prop<T>` to its
/// `prop_id` is the macro's job — ABI §5.2 fixes the id as the field's position, and a
/// caller writing `Prop::new(PropId::new(0))` is re-deriving something the macro already
/// knows and can get wrong. `eio-test-host` uses this so a test names the block's type and
/// nothing else.
pub trait Bound: Block {
    /// A fresh instance, every `Prop<T>` bound and every other field `Default`.
    fn bound() -> Self;
}

/// A typed handle on one of the block's properties (SDK §1).
///
/// Holds only its `prop_id` — ABI §5.2 fixes that as the property's position in the
/// manifest, and the `#[block]` macro binds it from field order, so the two cannot
/// disagree. There is no cached value: ABI §7.1 makes properties a *pull*, evaluated
/// host-side per signal on demand, and a guest-side cache would answer a question about a
/// signal the host has moved past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prop<T> {
    id: PropId,
    typed: PhantomData<fn() -> T>,
}

impl<T> Prop<T> {
    /// Binds a handle to a `prop_id`. Called by generated code.
    pub const fn new(id: PropId) -> Prop<T> {
        Prop {
            id,
            typed: PhantomData,
        }
    }

    /// The `prop_id` this handle reads (ABI §5.2).
    pub const fn id(self) -> PropId {
        self.id
    }
}

impl<T: FromValue> Prop<T> {
    /// Evaluates the property against a signal in the current batch (ABI §7.1).
    ///
    /// The index is the signal's position within *this* `process_signals` call's batch —
    /// ABI §7.1 is explicit that there is no hidden cursor.
    ///
    /// The grow-and-retry loop, the CBOR decode and the type check all happen here. A
    /// per-signal failure is `ERR_EXPR` for that call only and leaves the instance
    /// untouched: the block chooses to skip the signal, substitute a value, or route it to
    /// [`Out::ERR`](crate::Out::ERR).
    pub fn get(self, ctx: &mut Ctx, signal: u32) -> Result<T, BlockError> {
        self.read(ctx, SignalIdx::At(signal))
    }

    /// Evaluates the property with no signal context — ABI §3's `SIGNAL_NONE`.
    ///
    /// For `configure`, `start`, `stop` and the timer/gpio/http callbacks, where there is
    /// no batch. A *signal-dependent* expression answers `ERR_NO_SIGNAL_CONTEXT` here
    /// rather than a null (ABI §7.1), so a misconfiguration says so instead of producing a
    /// plausible wrong value.
    pub fn get_static(self, ctx: &mut Ctx) -> Result<T, BlockError> {
        self.read(ctx, SignalIdx::None)
    }

    fn read(self, ctx: &mut Ctx, signal: SignalIdx) -> Result<T, BlockError> {
        let value = ctx.prop(self.id, signal)?;
        T::from_value(value)
    }
}

/// A Rust type a [`Prop<T>`] can decode into (SDK §1.2).
///
/// The mapping is deliberately narrow and total: one Rust type per ABI §11.1 property
/// type, with no lossy conversions. A host has already checked the evaluated value against
/// the property's declared `type` and encoded it *as* that type (ABI §7.1: an int reaching
/// a `float` property arrives as a float), so by the time a value reaches here the only
/// way the type can be wrong is a manifest that declares one thing and a block that reads
/// another — a block-side mistake, reported as such.
pub trait FromValue: Sized {
    /// Decodes, or fails naming what arrived.
    fn from_value(value: Value) -> Result<Self, BlockError>;
}

/// The mismatch message, in one place so every type reports it the same way.
fn mismatch(expected: &str, got: &Value) -> BlockError {
    let actual = match got {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Bytes(_) => "bytes",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
    };
    BlockError::Decode(alloc::format!(
        "property evaluated to {actual}, but this block reads it as {expected}; \
         the manifest's declared type and the field's Rust type disagree (ABI §11.1)"
    ))
}

impl FromValue for bool {
    fn from_value(value: Value) -> Result<bool, BlockError> {
        match value {
            Value::Bool(value) => Ok(value),
            other => Err(mismatch("bool", &other)),
        }
    }
}

impl FromValue for i64 {
    fn from_value(value: Value) -> Result<i64, BlockError> {
        match value {
            Value::Int(value) => Ok(value),
            other => Err(mismatch("int", &other)),
        }
    }
}

impl FromValue for f64 {
    fn from_value(value: Value) -> Result<f64, BlockError> {
        match value {
            Value::Float(value) => Ok(value),
            // Deliberately NOT accepting `Value::Int` here. ABI §11.1 promotes an exact int
            // to a `float` property *host-side* and requires the host to encode it as a
            // float, precisely so a guest never has to handle both. An int arriving at a
            // `float` field therefore means the manifest declared `int`, and silently
            // converting would hide that disagreement rather than report it.
            other => Err(mismatch("float", &other)),
        }
    }
}

impl FromValue for alloc::string::String {
    fn from_value(value: Value) -> Result<alloc::string::String, BlockError> {
        match value {
            Value::Str(value) => Ok(value),
            other => Err(mismatch("string", &other)),
        }
    }
}

impl FromValue for alloc::vec::Vec<u8> {
    fn from_value(value: Value) -> Result<alloc::vec::Vec<u8>, BlockError> {
        match value {
            Value::Bytes(value) => Ok(value),
            other => Err(mismatch("bytes", &other)),
        }
    }
}

impl FromValue for Value {
    /// `any` (ABI §11.1) — whatever the expression produced, unexamined.
    fn from_value(value: Value) -> Result<Value, BlockError> {
        Ok(value)
    }
}

/// Markers naming ABI §11.1's property types at the type level (SDK §1.2).
///
/// These exist so the `#[block]` macro can turn `ty = "float"` into a *type* and have the
/// compiler check it against the field's `Prop<T>`.
pub mod ty {
    /// ABI §11.1 `bool`.
    pub struct Bool;
    /// ABI §11.1 `int`.
    pub struct Int;
    /// ABI §11.1 `float`.
    pub struct Float;
    /// ABI §11.1 `string`.
    pub struct Str;
    /// ABI §11.1 `bytes`.
    pub struct Bytes;
    /// ABI §11.1 `any`.
    pub struct Any;
}

/// "This Rust type is what ABI §11.1's `D` decodes to" (SDK §1.2).
///
/// The mapping is one-to-one and closed. There is no `impl` making an `i64` field satisfy
/// a `float` property: ABI §11.1's int-to-float promotion is the *host's*, applied to an
/// evaluated value and encoded as a float, so a guest declaring `float` always receives a
/// float. A field typed `i64` against `ty = "float"` is not a conversion to perform, it is
/// two statements about the same property that disagree.
pub trait Declared<D> {}

impl Declared<ty::Bool> for bool {}
impl Declared<ty::Int> for i64 {}
impl Declared<ty::Float> for f64 {}
impl Declared<ty::Str> for alloc::string::String {}
impl Declared<ty::Bytes> for alloc::vec::Vec<u8> {}
// `any` is ABI §11.1's "any value in the §6.3 space", and `Value` is that space. A
// concretely-typed field would be claiming to know something `any` does not promise.
impl Declared<ty::Any> for Value {}

/// The bound the macro checks a `#[prop]` field against.
///
/// Blanket-implemented, so the error a block author sees names the *field's* type rather
/// than an implementation detail: `the trait bound `Prop<f64>: PropDeclared<ty::Int>` is
/// not satisfied`.
pub trait PropDeclared<D> {}

impl<D, T: Declared<D>> PropDeclared<D> for Prop<T> {}
