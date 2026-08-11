//! [`Ctx`] — the only channel to the host (SDK §2), and the ABI §5.2 descriptor it is
//! built from.
//!
//! Every `eio:core` function (ABI §7.0) reaches a block through a method here, and nothing
//! else does. A block never sees a pointer, a length, a status code, or a grow-and-retry
//! loop; it sees `&[u8]`, `Batch`, `Value`, and `Result`.
//!
//! Capability wrappers — `state`, `timer`, `gpio`, `i2c`, `http` — are **not** here. They
//! arrive with eieio-7d8.3, gated on the manifest's declared capabilities (SDK §3).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use eio_abi::{ErrorCode, Level, PORT_ERR, SIGNAL_NONE, Size, Status};
use eio_signal::{Batch, Value};

use crate::error::{BlockError, HostError};
use crate::raw;

/// An output port to emit on (ABI §6.2).
///
/// A newtype rather than a bare `u32` so that a port index and a property id — both `u32`
/// at the ABI — cannot be swapped at a call site. eieio-7d8.2 generates a typed `Out` enum
/// per block from the macro's `outputs(..)`, which makes emitting to an undeclared port a
/// compile error (SDK §1); this is the untyped form that enum lowers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Out(u32);

impl Out {
    /// The reserved error port, which every block has without declaring it (ABI §6.4).
    pub const ERR: Out = Out(PORT_ERR);

    /// The output port at `index` in the descriptor's `outputs` (ABI §5.2).
    pub const fn new(index: u32) -> Out {
        Out(index)
    }

    /// The `u32` the ABI carries.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A property, by the `prop_id` its position in the manifest fixes (ABI §5.2, §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PropId(u32);

impl PropId {
    /// The property at `index` in the descriptor's `props` (ABI §5.2).
    pub const fn new(index: u32) -> PropId {
        PropId(index)
    }

    /// The `u32` the ABI carries.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Which signal a property is evaluated against (ABI §7.1).
///
/// Named for ABI §7.1's own `signal_idx` parameter, and deliberately **not** `Signal`:
/// ABI §2 fixes that word for one CBOR map, which is [`eio_signal::Signal`] and is what a
/// block author means by it. A prelude that bound `Signal` to "which signal, or none"
/// would shadow the platform's settled vocabulary with a different idea in the one place
/// every block imports from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalIdx {
    /// A signal's position within the batch of the *current* `eio_process_signals` call.
    ///
    /// Explicitly, with no hidden cursor: ABI §7.1 numbers signals within this call only,
    /// and an index carried out of the callback would answer a different question.
    At(u32),
    /// `SIGNAL_NONE` — no signal context (ABI §3).
    ///
    /// What `configure`, `start`, `stop` and the timer/gpio/http callbacks pass. A
    /// signal-*dependent* expression evaluated this way is `ERR_NO_SIGNAL_CONTEXT`, never
    /// a null value.
    None,
}

impl SignalIdx {
    const fn as_i32(self) -> i32 {
        match self {
            SignalIdx::At(index) => index as i32,
            SignalIdx::None => SIGNAL_NONE as i32,
        }
    }
}

/// The limits the host published for this instance (ABI §5.2, §9.7).
///
/// **Neither has a floor.** ABI §9.7 makes both host configuration and says a block "may
/// assume nothing about their size" — an MCU host may publish numbers a server host would
/// consider unusably small. A block reads them here and honours them; it does not
/// hard-code a size it believes is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest `(ptr, len)` the host will accept or deliver (ABI §9.7).
    pub max_payload: u64,
    /// The largest signal count per batch (ABI §9.7).
    pub max_batch: u64,
}

/// The instance descriptor, as delivered to `eio_configure` (ABI §5.2).
///
/// Properties are deliberately absent — they are pulled through [`Ctx::prop`] (ABI §7.1),
/// not pushed here. What this carries is the *shape* of the instance: the names whose
/// positions define port indices and `prop_id`s, fixed for the instance's life, so a block
/// resolves names to indices once here and uses indices at run time (ABI §5.2: MCUs do not
/// hash strings per signal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// Unique within the service.
    pub instance_id: String,
    /// The block ref (registry name).
    pub block: String,
    /// Input port names; position in the list is the port index.
    pub inputs: Vec<String>,
    /// Output port names; position in the list is the port index.
    pub outputs: Vec<String>,
    /// Property names; position in the list is the `prop_id`.
    pub props: Vec<String>,
    /// What this host will carry (ABI §9.7).
    pub limits: Limits,
}

impl Descriptor {
    /// Decodes the CBOR document `eio_configure` was handed (ABI §5.2).
    pub fn from_cbor(bytes: &[u8]) -> Result<Descriptor, BlockError> {
        let value = Value::from_cbor(bytes)?;
        let Value::Map(map) = value else {
            return Err(BlockError::Decode("descriptor is not a map".into()));
        };

        let Some(Value::Map(limits)) = map.get("limits") else {
            return Err(BlockError::Decode(
                "descriptor is missing \"limits\"".into(),
            ));
        };

        Ok(Descriptor {
            instance_id: text(&map, "instance_id")?,
            block: text(&map, "block")?,
            inputs: names(&map, "inputs")?,
            outputs: names(&map, "outputs")?,
            props: names(&map, "props")?,
            limits: Limits {
                max_payload: limit(limits, "max_payload")?,
                max_batch: limit(limits, "max_batch")?,
            },
        })
    }

    /// The index of the output port called `name`, if the instance has one.
    ///
    /// Resolve once in `configure`; use the index thereafter (ABI §5.2).
    pub fn output(&self, name: &str) -> Option<Out> {
        position(&self.outputs, name).map(Out::new)
    }

    /// The index of the input port called `name`, if the instance has one.
    pub fn input(&self, name: &str) -> Option<u32> {
        position(&self.inputs, name)
    }

    /// The `prop_id` of the property called `name`, if the instance has one.
    pub fn prop(&self, name: &str) -> Option<PropId> {
        position(&self.props, name).map(PropId::new)
    }
}

fn text(map: &eio_signal::Map, key: &str) -> Result<String, BlockError> {
    match map.get(key) {
        Some(Value::Str(text)) => Ok(text.clone()),
        Some(_) => Err(BlockError::Decode(alloc::format!(
            "descriptor field {key:?} is not a text string"
        ))),
        None => Err(BlockError::Decode(alloc::format!(
            "descriptor is missing {key:?}"
        ))),
    }
}

fn names(map: &eio_signal::Map, key: &str) -> Result<Vec<String>, BlockError> {
    // Absent is empty: a block with no inputs, outputs or properties is ordinary.
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = value else {
        return Err(BlockError::Decode(alloc::format!(
            "descriptor field {key:?} is not an array"
        )));
    };
    items
        .iter()
        .map(|item| match item {
            Value::Str(text) => Ok(text.clone()),
            _ => Err(BlockError::Decode(alloc::format!(
                "descriptor field {key:?} holds a non-string name"
            ))),
        })
        .collect()
}

fn limit(map: &eio_signal::Map, key: &str) -> Result<u64, BlockError> {
    match map.get(key) {
        // ABI §6.3 makes every integer a signed 64-bit one, so a limit arrives as
        // an `Int`. Negative is not a small limit, it is a host that got its own
        // descriptor wrong, and silently clamping it to zero would present that as
        // "this host carries nothing".
        Some(Value::Int(value)) if *value >= 0 => Ok(*value as u64),
        Some(Value::Int(value)) => Err(BlockError::Decode(alloc::format!(
            "descriptor limit {key:?} is negative ({value})"
        ))),
        Some(_) => Err(BlockError::Decode(alloc::format!(
            "descriptor limit {key:?} is not an integer"
        ))),
        None => Err(BlockError::Decode(alloc::format!(
            "descriptor limits are missing {key:?}"
        ))),
    }
}

fn position(names: &[String], wanted: &str) -> Option<u32> {
    names
        .iter()
        .position(|name| name == wanted)
        .map(|index| index as u32)
}

/// The block's channel to the host (SDK §2).
///
/// Constructed by the generated exports (eieio-7d8.2) and handed to every callback. A block
/// never builds one.
#[derive(Debug)]
pub struct Ctx {
    limits: Limits,
    /// The buffer `prop` grows (ABI §7.1). Kept across calls rather than allocated per
    /// call: the host caches the evaluation for the duration of the callback, so the retry
    /// is cheap, and a block reading the same property for every signal in a batch would
    /// otherwise allocate once per signal.
    prop_buffer: Vec<u8>,
}

impl Ctx {
    /// Builds the context for an instance with these limits.
    pub fn new(limits: Limits) -> Ctx {
        Ctx {
            limits,
            prop_buffer: Vec::new(),
        }
    }

    /// What this host will carry (ABI §5.2, §9.7).
    ///
    /// Neither limit has a floor. Read them; do not assume them.
    pub const fn limits(&self) -> Limits {
        self.limits
    }

    /// Writes a message to the host log (ABI §7.0).
    ///
    /// Takes a [`Level`] rather than the wire number, so a call site cannot pass a `3`
    /// meaning something other than `warn`. [`crate::log`]'s macros route here, so a block
    /// author normally writes `log::info!` rather than calling this.
    pub fn log(&mut self, level: Level, message: &str) {
        raw::log(level, message);
    }

    /// Enqueues `batch` on `port` (ABI §6.2).
    ///
    /// **Enqueue, not delivery.** The host buffers the batch and routes it after this
    /// callback returns, which is why emitting cannot recurse into this instance or any
    /// other. Fan-out, backpressure and tapping are all invisible from here.
    ///
    /// The batch is encoded to canonical CBOR (ABI §6.3.1) and its length checked against
    /// [`Limits::max_payload`] before the call — ABI §6.2's third refusal, and one of the
    /// three whose code the spec fixes so that a guest hears the same answer from every
    /// host.
    ///
    /// [`Limits::max_batch`] is deliberately **not** checked here. ABI §6.2's table has
    /// three entries and the signal count is not one of them; §9.7's operative sentence
    /// about `max_batch` is that a host "never delivers batches beyond" it, which is the
    /// inbound direction. Refusing locally would report an `ERR_LIMIT` no host produced —
    /// inventing a fourth refusal in the one place §6.2 says the answer must not vary.
    /// Whether `max_batch` bounds emissions at all is a genuine gap in the spec, filed as
    /// eieio-7d8.13; the limit is on [`Ctx::limits`] so a block that wants to respect it
    /// can.
    pub fn emit(&mut self, port: Out, batch: &Batch) -> Result<(), BlockError> {
        // `encoded_len` is exact — `to_cbor` uses it to presize its own buffer — so an
        // oversized batch is refused without paying for the encode that would be thrown
        // away. `emit_cbor` checks again, because it is reachable on its own.
        if batch.encoded_len() as u64 > self.limits.max_payload {
            return Err(HostError::new("emit", ErrorCode::Limit).into());
        }
        self.emit_cbor(port, &batch.to_cbor())
    }

    /// Enqueues an already-encoded canonical batch on `port` (ABI §6.2).
    ///
    /// The zero-copy path: a block forwarding the payload it was delivered has canonical
    /// bytes already, and re-encoding them would spend a decode and an encode to reproduce
    /// what it holds (ABI §6.3.1 guarantees the round trip is byte-for-byte, so it really
    /// would be the same bytes).
    pub fn emit_cbor(&mut self, port: Out, batch: &[u8]) -> Result<(), BlockError> {
        if batch.len() as u64 > self.limits.max_payload {
            return Err(HostError::new("emit", ErrorCode::Limit).into());
        }
        let status = raw::emit(port.index() as i32, batch);
        match Status::decode(status) {
            Status::Ok => Ok(()),
            Status::Failed(code) => Err(HostError::new("emit", code).into()),
        }
    }

    /// Evaluates a property against a signal, host-side (ABI §7.1).
    ///
    /// Hides the grow-and-retry loop of ABI §8's size convention: the buffer is grown to
    /// whatever the host said it needed and the call is repeated. The host caches the
    /// evaluation for the duration of this callback, so the retry does not re-evaluate.
    ///
    /// The value satisfies the property's declared `type` (ABI §11.1) — an `int` reaching a
    /// `float` property arrives as a float, so a block decodes what was declared and never
    /// has to handle both.
    pub fn prop(&mut self, id: PropId, signal: SignalIdx) -> Result<Value, BlockError> {
        let bytes = self.prop_cbor(id, signal)?;
        Ok(Value::from_cbor(bytes)?)
    }

    /// [`Ctx::prop`] without the decode, for a caller that wants the canonical bytes.
    ///
    /// Borrows `self` for the lifetime of the result, because the bytes live in the
    /// context's reusable buffer.
    pub fn prop_cbor(&mut self, id: PropId, signal: SignalIdx) -> Result<&[u8], BlockError> {
        // Start from whatever the buffer already grew to. A block reading one property per
        // signal converges on the right size after the first signal and never grows again.
        if self.prop_buffer.is_empty() {
            self.prop_buffer.resize(64, 0);
        }

        let written = loop {
            let cap = self.prop_buffer.len();
            let returned = raw::prop(id.index() as i32, signal.as_i32(), &mut self.prop_buffer);
            match Size::decode(returned, cap) {
                Size::Written(bytes) => break bytes,
                // ABI §8: nothing was written and this many bytes are needed. Grow to
                // exactly that and ask again; the host's answer is authoritative, so there
                // is no second retry to bound.
                Size::Required(bytes) => self.prop_buffer.resize(bytes, 0),
                Size::Failed(code) => return Err(HostError::new("prop", code).into()),
            }
        };

        // `truncate` keeps the capacity, which is what makes the buffer converge: the next
        // call starts from the size the last answer needed.
        self.prop_buffer.truncate(written);
        Ok(&self.prop_buffer[..written])
    }

    /// Sends structured detail to accompany a non-zero callback return (ABI §7.0, §8).
    ///
    /// Called by the generated exports (eieio-7d8.2) when a callback returns `Err`; a block
    /// does not normally call it directly.
    pub fn error(&mut self, error: &BlockError) {
        let detail = alloc::format!("{error}");
        raw::error(error.code().as_i32(), &detail);
    }

    /// Wall-clock milliseconds since the Unix epoch (ABI §7.0).
    ///
    /// Host-mediated deliberately: the clock is a determinism and replay lever, so a guest
    /// does not get to read one of its own.
    pub fn time_unix_ms(&mut self) -> i64 {
        raw::time_unix_ms()
    }

    /// Monotonic milliseconds (ABI §7.0).
    ///
    /// No fixed origin, and it does not survive the instance: ABI §5.1 makes a restart a
    /// fresh instance, so this starts again from wherever the new one's host says.
    pub fn time_mono_ms(&mut self) -> i64 {
        raw::time_mono_ms()
    }

    /// Fills `buffer` with host randomness (ABI §7.0).
    ///
    /// Uses the *status* convention rather than the size one — the parameter is a `len`,
    /// not a `cap` — so this is all-or-nothing and there is no short answer to retry from.
    pub fn rand(&mut self, buffer: &mut [u8]) -> Result<(), BlockError> {
        if buffer.is_empty() {
            return Ok(());
        }
        let status = raw::rand(buffer);
        match Status::decode(status) {
            Status::Ok => Ok(()),
            Status::Failed(code) => Err(HostError::new("rand", code).into()),
        }
    }

    /// `n` bytes of host randomness (ABI §7.0).
    pub fn rand_bytes(&mut self, n: usize) -> Result<Vec<u8>, BlockError> {
        let mut buffer = vec![0u8; n];
        self.rand(&mut buffer)?;
        Ok(buffer)
    }
}
