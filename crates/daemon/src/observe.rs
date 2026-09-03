//! The observation bus (DAEMON-SPEC §11), and the taps that read it (§6.3, §9.6).
//!
//! # Why this exists at all, before it is a feature
//!
//! Each instance reports what it observed on an **unbounded** channel (DAEMON §5) — unbounded
//! because an observer that could stall a guest by reading slowly would be a worse defect than
//! a queue that grows. Unbounded means something has to read it, and in a node that something
//! is [`Bus`]: one drain task per instance, forwarding into a broadcast that taps and
//! `/logs/stream` subscribe to. A node with no subscribers still drains, and drops.
//!
//! # Zero cost untapped
//!
//! DAEMON §6.3's claim, and it is a claim about the *emit path*, which this module deliberately
//! does not touch. An instance already reports what it emitted whether or not anyone is
//! listening, so tapping adds nothing to routing: the drain checks
//! [`receiver_count`](tokio::sync::broadcast::Sender::receiver_count) — one atomic load — and
//! with no subscribers does not clone the batch, allocate, or take a lock. Nothing a service
//! does is conditional on a tap existing.
//!
//! # The ring is the broadcast channel's, and lag is reported
//!
//! A slow reader is bounded by [`RING`], and `broadcast` answers it `Lagged(n)` with the exact
//! number it missed. That number is DAEMON §9.6's sampling report, forwarded in-stream: a
//! debugging tool that quietly showed a subset would be worse than one that shows less and
//! says so.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::sync::{Mutex, broadcast};
use utoipa::ToSchema;

use crate::core_fns::Emission;
use crate::executor::{Event, Events};
use crate::router::DiscardReason;

/// How many observations the bus holds for a reader that is behind (DAEMON §9.6).
///
/// A browser watching a firehose should see recent signals rather than stall the node, and
/// what it missed is counted rather than hidden. Sized for a human reader on a slow link: big
/// enough that an ordinary service never laps it, small enough that the memory is a rounding
/// error beside one wasmtime instance.
const RING: usize = 1024;

/// Where an observation came from, and what it was (DAEMON §9.6).
///
/// Serialized as the SSE event's data. The port travels as a **name** rather than an index:
/// the drain resolves it once from the instance's descriptor, so a tap requested as
/// `"t1.out -> t2.in"` can be matched without the API knowing anything about port numbering.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Observation {
    /// The service the instance belongs to.
    pub service: String,
    /// The instance's id (SERVICE §2).
    pub instance: String,
    /// The SSE event name this is published as (§9.6).
    ///
    /// Always [`What::event`] for the `what` beside it — every construction site sets
    /// `event: what.event()`, so this can never name a different variant than the one that
    /// actually travels with it. Stored rather than recomputed at serialize time because two
    /// readers need the name *before* they have JSON to look inside: [`crate::api::sse::stream_of`]
    /// names the SSE frame's `event:` line with it, and `crate::api::logs` filters on it — both
    /// without reaching into `what` to re-derive what is already sitting right here.
    pub event: &'static str,
    /// When this was observed, RFC 3339 with milliseconds.
    ///
    /// **The daemon's clock, not the reader's.** DESIGNER §6 renders a line as
    /// `[timestamp][LEVEL][service.block]`, and a client stamping arrival time would be wrong
    /// in the two cases that matter: a reader behind enough to be told it `Lagged` is reading
    /// events later than they happened, and a backlog replayed before the live stream is
    /// joined (§6's nio-logger virtue) would be stamped as if it had just occurred. Only the
    /// daemon knows when the thing happened.
    pub at: String,
    /// The output port, when the observation has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// What was seen.
    #[serde(flatten)]
    pub what: What,
}

/// The body of an [`Observation`], by event name (DAEMON §9.6).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum What {
    /// A batch that travelled the tapped connection, rendered as EXPR §7.6 canonical text.
    ///
    /// Rendered rather than re-encoded because a stream is read by people and by agents, and
    /// the canonical rendering is the one definition of what a value looks like — a second one
    /// here would be a second answer the conformance vectors do not pin.
    Signals {
        /// One rendering per signal in the batch.
        signals: Vec<String>,
    },
    /// A property expression that failed for a signal (EXPR §8) — the payoff of §6.3.
    ExprFailure {
        /// EXPR §8's error code, which is what a caller branches on.
        code: String,
        /// Where in the property's source text, as `start..end` byte offsets.
        span: String,
        /// The message, for a person.
        message: String,
        /// Which property, by the index the descriptor numbers it with.
        prop: u32,
        /// Which signal of the batch, when the failure was per-signal.
        #[serde(skip_serializing_if = "Option::is_none")]
        signal: Option<u32>,
    },
    /// A batch that was routed and not delivered (DAEMON §6.2).
    Discarded {
        /// Why.
        reason: String,
    },
    /// A log line, from the daemon or from a guest (ABI §7.0, DAEMON §11).
    Log {
        /// `trace`..`error`.
        level: String,
        /// The line.
        message: String,
    },
    /// The stream skipped observations because this reader was behind (DAEMON §9.6).
    Lagged {
        /// Exactly how many. The sampling report.
        missed: u64,
    },
}

impl What {
    /// The SSE event name this variant is published as (DAEMON §9.6).
    ///
    /// The one definition of the variant→event-name mapping. Every construction site
    /// (`Bus::log`, `observe`, and `api::sse::stream_of`'s own `Lagged`) calls this rather than
    /// naming the event a second time, so [`Observation::event`] cannot drift from `what`, and a
    /// checker (eieio-m9s.13's SSE schema-parity test) can read the mapping off this match rather
    /// than a hand-typed table beside it. Exhaustive on purpose, with no wildcard arm: a variant
    /// added to `What` without a matching arm here fails the build, not silently falls through.
    pub fn event(&self) -> &'static str {
        match self {
            What::Signals { .. } => event::SIGNALS,
            What::ExprFailure { .. } => event::EXPR_FAILURE,
            What::Discarded { .. } => event::DISCARDED,
            What::Log { .. } => event::LOG,
            What::Lagged { .. } => event::LAGGED,
        }
    }
}

/// The per-node bus every observation passes through (DAEMON §11).
#[derive(Debug)]
pub struct Bus {
    sender: broadcast::Sender<Arc<Observation>>,
    /// How many observations have been published, for the untapped-cost proof.
    published: AtomicU64,
    /// How many were dropped because nothing was listening — the untapped path.
    unwatched: AtomicU64,
    /// How many events came off an instance's stream, published or not.
    ///
    /// The leak proof: an unbounded channel that nothing reads is DAEMON §11's whole reason
    /// for this type, and a counter that stays at zero while a service runs means the drain
    /// is not running.
    drained: AtomicU64,
    /// The taps this node is holding (§9.6).
    taps: Mutex<BTreeMap<String, Tap>>,
    /// Mints tap ids. Monotonic rather than random: an id is a handle within one process.
    next_tap: AtomicU64,
}

impl Default for Bus {
    fn default() -> Bus {
        Bus {
            sender: broadcast::channel(RING).0,
            published: AtomicU64::new(0),
            unwatched: AtomicU64::new(0),
            drained: AtomicU64::new(0),
            taps: Mutex::new(BTreeMap::new()),
            next_tap: AtomicU64::new(1),
        }
    }
}

/// One registered tap (DAEMON §9.6).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Tap {
    /// The handle `GET /taps/{id}/stream` and `DELETE /taps/{id}` address it by.
    pub id: String,
    /// The service the tapped connection is in.
    pub service: String,
    /// The connection, as the service file spells it: `"from.port -> to.port"` (SERVICE §5).
    pub connection: String,
    /// The instance the observations come from — the connection's source (§6.3).
    pub instance: String,
    /// The output port they come from.
    pub port: String,
}

impl Bus {
    /// Publishes one observation, unless nothing is listening.
    ///
    /// The `receiver_count` check is DAEMON §6.3's "zero cost untapped": one atomic load, and
    /// with no subscribers no `Arc` is allocated and no batch is cloned.
    fn publish(&self, observation: impl FnOnce() -> Observation) {
        if self.sender.receiver_count() == 0 {
            self.unwatched.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.published.fetch_add(1, Ordering::Relaxed);
        let _ = self.sender.send(Arc::new(observation()));
    }

    /// A subscription to everything published from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Observation>> {
        self.sender.subscribe()
    }

    /// What the bus has seen: drained, published, and dropped unwatched.
    ///
    /// `#[cfg(test)]` because these numbers exist to *prove* two claims rather than to serve
    /// anyone: nothing in the daemon reads them, and the honest place for counters an operator
    /// can see is `/metrics`, which SCOPE §3.12 leaves OPEN (DAEMON §11).
    #[cfg(test)]
    ///
    /// Two proofs in three numbers. `drained` rising while a service runs is DAEMON §11's
    /// leak fix — the unbounded channel is being read. `published` staying at zero with no
    /// subscriber is §6.3's zero-cost-untapped, and it means no batch was cloned.
    pub fn counts(&self) -> Counts {
        Counts {
            drained: self.drained.load(Ordering::Relaxed),
            published: self.published.load(Ordering::Relaxed),
            unwatched: self.unwatched.load(Ordering::Relaxed),
        }
    }

    /// Registers a tap on `connection` in `service`, resolved to its source endpoint (§6.3).
    pub async fn tap(&self, service: &str, connection: &str, instance: &str, port: &str) -> Tap {
        let id = format!("t{}", self.next_tap.fetch_add(1, Ordering::Relaxed));
        let tap = Tap {
            id: id.clone(),
            service: String::from(service),
            connection: String::from(connection),
            instance: String::from(instance),
            port: String::from(port),
        };
        self.taps.lock().await.insert(id, tap.clone());
        tap
    }

    /// The tap with that id.
    pub async fn tap_of(&self, id: &str) -> Option<Tap> {
        self.taps.lock().await.get(id).cloned()
    }

    /// Every tap this node holds, in id order.
    pub async fn taps(&self) -> Vec<Tap> {
        self.taps.lock().await.values().cloned().collect()
    }

    /// Removes a tap. `None` if there was no such one.
    pub async fn untap(&self, id: &str) -> Option<Tap> {
        self.taps.lock().await.remove(id)
    }

    /// Publishes a log line (DAEMON §11).
    pub fn log(&self, service: &str, instance: &str, level: &str, message: &str) {
        self.publish(|| {
            let what = What::Log {
                level: String::from(level),
                message: String::from(message),
            };
            Observation {
                at: now_rfc3339(),
                service: String::from(service),
                instance: String::from(instance),
                event: what.event(),
                port: None,
                what,
            }
        });
    }
}

/// Now, RFC 3339 with milliseconds — the stamp every [`Observation`] carries.
///
/// Milliseconds and not nanoseconds because DESIGNER §6 renders this for a person and a log
/// panel showing nine fractional digits is noise; and formatted rather than an epoch integer
/// because every reader of it so far is a UI, and a UI that has to know the epoch is a UI that
/// will get it wrong once.
pub(crate) fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;

    let now = time::OffsetDateTime::now_utc();
    // Truncate to whole milliseconds. `replace_nanosecond` only fails out of range, and this
    // value came from a nanosecond that was already in range, so it cannot.
    now.replace_nanosecond(now.millisecond() as u32 * 1_000_000)
        .unwrap_or(now)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// See [`Bus::counts`].
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Events taken off instance streams.
    pub drained: u64,
    /// Observations sent to subscribers.
    pub published: u64,
    /// Observations there was nobody to send.
    pub unwatched: u64,
}

/// The SSE event names (DAEMON §9.6). A client dispatches on these.
pub mod event {
    /// A batch that travelled the tapped connection.
    pub const SIGNALS: &str = "signals";
    /// A property expression that failed for a signal (EXPR §8).
    pub const EXPR_FAILURE: &str = "expr_failure";
    /// A batch routed and not delivered (DAEMON §6.2).
    pub const DISCARDED: &str = "discarded";
    /// A log line (DAEMON §11).
    pub const LOG: &str = "log";
    /// The stream skipped observations for this reader (§9.6).
    pub const LAGGED: &str = "lagged";
}

/// Drains one instance's event stream into `bus`, until the instance ends (DAEMON §11).
///
/// Spawned per instance by [`Service::spawn`](crate::router::Service::spawn) when a node is
/// running one. The `outputs` table is the instance's descriptor's output port names, so an
/// index becomes a name once here rather than at every reader.
pub fn drain(
    bus: Arc<Bus>,
    service: String,
    instance: String,
    outputs: Vec<String>,
    mut events: Events,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            bus.drained.fetch_add(1, Ordering::Relaxed);
            observe(&bus, &service, &instance, &outputs, event);
        }
    });
}

/// Turns one [`Event`] into an [`Observation`], or drops it.
///
/// Not every event is worth a stream: a callback returning `Ok` is what is supposed to happen,
/// and publishing it would bury the four things an operator opened a tap to see.
fn observe(bus: &Bus, service: &str, instance: &str, outputs: &[String], event: Event) {
    let port_name = |port: u32| {
        outputs.get(port as usize).cloned().unwrap_or_else(|| {
            match port == eio_host_core::PORT_ERR {
                true => String::from(eio_manifest::PORT_ERR_NAME),
                false => port.to_string(),
            }
        })
    };

    match event {
        Event::Emitted {
            emission: Emission { port, batch },
            ..
        } => {
            // Everything expensive — resolving the port's name, rendering the batch — happens
            // *inside* the closure, so an untapped node does none of it (DAEMON §6.3).
            bus.publish(|| {
                let what = What::Signals {
                    // EXPR §7.6's canonical rendering, the same one `dev run-block` prints:
                    // a second definition of what a value looks like would be a second answer
                    // the conformance vectors do not pin.
                    signals: batch
                        .iter()
                        .map(|signal| eio_expr::render(signal.as_value()))
                        .collect(),
                };
                Observation {
                    at: now_rfc3339(),
                    service: String::from(service),
                    instance: String::from(instance),
                    event: what.event(),
                    port: Some(port_name(port)),
                    what,
                }
            });
        }
        Event::Failure(failure) => bus.publish(|| {
            let what = What::ExprFailure {
                code: format!("{:?}", failure.error.code),
                span: format!("{}..{}", failure.error.span.start, failure.error.span.end),
                message: failure.error.to_string(),
                prop: failure.prop_id,
                signal: failure.signal,
            };
            Observation {
                at: now_rfc3339(),
                service: String::from(service),
                instance: String::from(instance),
                event: what.event(),
                port: None,
                what,
            }
        }),
        Event::Discarded(discard) => {
            bus.publish(|| {
                let what = What::Discarded {
                    reason: String::from(reason_of(discard.reason)),
                };
                Observation {
                    at: now_rfc3339(),
                    service: String::from(service),
                    instance: String::from(instance),
                    event: what.event(),
                    port: Some(port_name(discard.port)),
                    what,
                }
            });
        }
        // Statuses, details, refusals, deaths and stops are the log's (DAEMON §11) rather than
        // a tap's: they are about the instance, not about what travelled a connection.
        _ => {}
    }
}

/// A discard reason, as the stream names it.
///
/// The enum's own `Display` is a sentence for an operator's log; a stream needs a slug a client
/// can branch on, for the reason DAEMON §9.2 gives about every other error this API reports.
fn reason_of(reason: DiscardReason) -> &'static str {
    match reason {
        DiscardReason::Unrouted => "unrouted",
        DiscardReason::Overflow => "overflow",
        DiscardReason::SelfFull => "self_full",
        DiscardReason::Gone => "gone",
    }
}

/// A `tracing` layer that publishes every log line onto a bus (DAEMON §11).
///
/// Fed by the same span the guest's `log` calls are tagged with, so a block's line and the
/// daemon's own line about that block arrive with the same `(service, instance)` and
/// `/logs/stream` can filter both by one pair.
///
/// Holds its bus rather than reaching for a global one, which is what makes it testable: a
/// global would admit exactly one bus per process, so the wiring from a span to a stream could
/// only ever be exercised by the one node the process is running.
pub struct LogLayer(Arc<Bus>);

impl LogLayer {
    /// A layer publishing onto `bus`.
    pub fn new(bus: Arc<Bus>) -> LogLayer {
        LogLayer(bus)
    }
}

/// The identity a span carries, stashed when the span is created.
///
/// `tracing` does not keep a span's field values for later readers, so they are recorded here
/// at creation and looked up when an event fires inside the span.
#[derive(Debug, Clone, Default)]
struct Identity {
    service: String,
    instance: String,
}

impl<S> tracing_subscriber::Layer<S> for LogLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let Some(span) = context.span(id) else { return };
        let mut identity = Identity::default();
        attributes.record(&mut IdentityVisitor(&mut identity));
        if !identity.service.is_empty() || !identity.instance.is_empty() {
            span.extensions_mut().insert(identity);
        }
    }

    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // The nearest enclosing span that has one wins: an instance's span is inside the
        // node's, and the innermost is the most specific thing to attribute a line to.
        let mut identity = Identity::default();
        if let Some(scope) = context.event_scope(event) {
            for span in scope.from_root() {
                if let Some(found) = span.extensions().get::<Identity>() {
                    identity = found.clone();
                }
            }
        }

        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        if message.is_empty() {
            return;
        }
        self.0.log(
            &identity.service,
            &identity.instance,
            &event.metadata().level().to_string().to_ascii_lowercase(),
            &message,
        );
    }
}

/// Pulls `service` and `instance` out of a span's fields.
struct IdentityVisitor<'a>(&'a mut Identity);

impl tracing::field::Visit for IdentityVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        // `%value` on a `Display` field arrives here as its `Debug`, which for the
        // `tracing::field::DisplayValue` wrapper is the rendered string without quotes.
        let rendered = format!("{value:?}");
        match field.name() {
            "service" => self.0.service = rendered,
            "instance" => self.0.instance = rendered,
            _ => {}
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "service" => self.0.service = String::from(value),
            "instance" => self.0.instance = String::from(value),
            _ => {}
        }
    }
}

/// Pulls the `message` field out of an event, which is what `info!("...")` records it as.
struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            *self.0 = String::from(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects everything published while `body` runs.
    fn published(bus: &Arc<Bus>) -> tokio::sync::broadcast::Receiver<Arc<Observation>> {
        bus.subscribe()
    }

    #[tokio::test]
    async fn a_span_tagged_line_reaches_the_bus_tagged() {
        // DAEMON §11: a guest's `log` and the daemon's own line about that block carry the
        // same `(service, instance)`, taken from the span the lifecycle driver entered — so
        // `/logs/stream` can filter both by one pair. Driven through a real subscriber,
        // because what is under test is the wiring from a span's fields to an observation.
        use tracing_subscriber::layer::SubscriberExt as _;

        let bus = Arc::new(Bus::default());
        let mut seen = published(&bus);
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&bus)));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("instance", service = %"kitchen", instance = %"t1");
            span.in_scope(|| tracing::info!("the guest said something"));
            // Outside any instance span: still a line, with no identity to attribute it to.
            tracing::warn!("the node said something");
        });

        let tagged = seen.try_recv().expect("the tagged line");
        assert_eq!(tagged.service, "kitchen");
        assert_eq!(tagged.instance, "t1");
        assert_eq!(tagged.event, event::LOG);
        match &tagged.what {
            What::Log { level, message } => {
                assert_eq!(level, "info");
                assert_eq!(message, "the guest said something");
            }
            other => panic!("not a log: {other:?}"),
        }

        let untagged = seen.try_recv().expect("the node's own line");
        assert_eq!(
            untagged.service, "",
            "nothing to attribute it to, and it says so"
        );
        match &untagged.what {
            What::Log { level, .. } => assert_eq!(level, "warn"),
            other => panic!("not a log: {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_construction_site_stores_what_s_own_event_name() {
        // `Observation::event` is documented to always be `what.event()` — this is the
        // invariant that makes the mapping something a checker can read off the code (this
        // file's `What::event`) rather than trust a second, hand-typed list beside it
        // (eieio-m9s.13). Drives every reachable construction path and checks what actually
        // came out the other end, not just what the code is supposed to do.
        let bus = Arc::new(Bus::default());
        let mut seen = published(&bus);

        bus.log("kitchen", "t1", "info", "a log line");
        observe(
            &bus,
            "kitchen",
            "t1",
            &[String::from("out")],
            Event::Emitted {
                callback: "process_signals",
                emission: Emission {
                    port: 0,
                    batch: eio_signal::Batch::new(),
                },
            },
        );
        observe(
            &bus,
            "kitchen",
            "t1",
            &[String::from("out")],
            Event::Failure(eio_host_core::PropFailure {
                prop_id: 0,
                signal: None,
                error: eio_expr::Error::new(
                    eio_expr::ErrorCode::Parse,
                    eio_expr::Span::new(0, 1),
                    "boom",
                ),
            }),
        );
        observe(
            &bus,
            "kitchen",
            "t1",
            &[String::from("out")],
            Event::Discarded(crate::router::Discard {
                port: 0,
                reason: DiscardReason::Unrouted,
            }),
        );

        let mut count = 0;
        while let Ok(observation) = seen.try_recv() {
            assert_eq!(
                observation.event,
                observation.what.event(),
                "stored `event` drifted from `what.event()`: {observation:?}"
            );
            count += 1;
        }
        assert_eq!(
            count, 4,
            "expected one observation per construction path exercised above"
        );
    }

    #[tokio::test]
    async fn a_line_with_nothing_listening_costs_no_allocation() {
        // §6.3's zero-cost claim at its narrowest: with no subscriber the bus does not build
        // an `Observation` at all, and the counters say which path was taken.
        let bus = Arc::new(Bus::default());
        bus.log("kitchen", "t1", "info", "nobody is watching");
        let counts = bus.counts();
        assert_eq!(counts.published, 0);
        assert_eq!(counts.unwatched, 1);

        let _listener = bus.subscribe();
        bus.log("kitchen", "t1", "info", "somebody is");
        assert_eq!(bus.counts().published, 1);
    }

    #[tokio::test]
    async fn a_tap_is_registered_until_it_is_removed() {
        let bus = Bus::default();
        let tap = bus.tap("kitchen", "t1.out -> t2.in", "t1", "out").await;
        assert_eq!(bus.taps().await.len(), 1);
        assert_eq!(
            bus.tap_of(&tap.id).await.map(|found| found.id),
            Some(tap.id.clone())
        );

        assert!(bus.untap(&tap.id).await.is_some());
        assert!(
            bus.taps().await.is_empty(),
            "teardown leaves no ring behind"
        );
        assert!(bus.untap(&tap.id).await.is_none());
    }
}
