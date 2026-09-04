//! The pub/sub bridge (DAEMON-SPEC §7): the normative boundary between a service graph and
//! whatever carries a batch to another node.
//!
//! # The boundary is the trait, and nothing above it
//!
//! [`Bridge`] is deliberately three things and nothing more: publish, subscribe, and a
//! connection's lifecycle ([`Bridge::is_connected`]). Nothing here or above it may name a
//! transport concept — DAEMON §7 in full: "the bridge is a normative boundary, not an
//! implementation convenience... nothing above it may name an MQTT concept". `crates/daemon`'s
//! part of that promise is mechanical, not a comment: [`tests::the_bridge_boundary_holds`]
//! scans every other module in this crate for the vocabulary a transport owns (QoS, retained
//! messages, `rumqttc`'s own types) and fails the build if it finds any. A second transport —
//! `rumqttc`'s, when it lands — is a second `impl Bridge` and changes nothing outside this file.
//!
//! # This module ships two transports: in-process, and MQTT
//!
//! [`InProcessBridge`] and the [`Broker`] behind it are not a stand-in for MQTT; they are a
//! transport in their own right, chosen so the whole cross-node flow — DAEMON §6.3's
//! `publisher`/`subscriber` blocks, §7's topics, SCOPE §3.4's at-most-once — can be proven with
//! no network in any test. Two [`InProcessBridge`] handles from one [`Broker`] behave, from
//! above the trait, exactly as two daemons on one MQTT broker would: a batch published on one
//! reaches every subscriber connected to the other, retains nothing, and drops rather than
//! blocks when it cannot be delivered. If a second transport could not satisfy that from behind
//! the same trait, the trait would be wrong; proving it holds for one transport is what
//! justifies writing the second.
//!
//! [`MqttBridge`] is that second transport (eieio-2vm.4): DAEMON §7 over `rumqttc`, real
//! sockets, QoS 0, never retained. `rumqttc` was deliberately absent until now (eieio-2vm.2)
//! because every API that carries a live connection needs either a broker process or the
//! network, and this crate's tests must have neither. What makes it testable without a
//! manually-started broker or a reachable non-loopback address (see
//! [`tests::mqtt_bridge_delivers_a_publish_across_a_real_broker`] and its neighbours) is
//! `rumqttd`, a dev-dependency: the same wire protocol as a library rather than a subprocess, so
//! a test spins one up on `127.0.0.1` and tears it down with the process — no fixture anyone has
//! to remember to start, no CI service container, no network beyond the loopback interface.
//! [`tests::mqtt_bridge_drops_and_counts_when_nothing_answers`] proves the loss path the same
//! way DAEMON §7.1 states it: nothing needs to be *running* to prove a bridge with no broker
//! drops rather than blocks, only a loopback address nothing is listening on.
//!
//! **Out of scope, by the issue that added this:** hosting a broker when nothing answers
//! (§7.1's "this node would host one" branch). A separate decision this transport does not
//! make — see [`MqttBridge::connect`]'s docs.
//!
//! # The bus key (SCOPE §3.11, DAEMON §7.1, eieio-2vm.5)
//!
//! [`MqttBridge::connect`] takes an optional `crate::pubsub::Key` and, when one is given,
//! presents it as this connection's MQTT credential — required by a broker candidate that has
//! one configured, ignored by one that does not. This raises the trust floor from "whichever
//! node answers on a configured address is trusted to be that node" to "anyone with the key",
//! and no further: no per-node identity, no revocation, and TLS stays out of this build
//! (`crates/daemon/Cargo.toml`'s forced no-`rustls` `rumqttc` features) because the MCU leaf
//! tier has to be able to do this too.
//!
//! [`dial`] tells apart the two ways a candidate can fail to become this bridge's broker: nobody
//! answered at all (DAEMON §7.1's ordinary "nothing reachable"), or somebody answered and closed
//! the connection during the handshake rather than completing it — the shape a wrong or missing
//! key takes on the wire, since the fixture broker these tests dial
//! ([`tests::spawn_embedded_broker`] with auth configured) never sends a failure `ConnAck`, only
//! silence followed by a closed socket. [`MqttBridge::rejected`] counts the second kind, kept
//! apart from [`MqttBridge::dropped`]'s count of publishes lost after connecting, so a bridge
//! that never connects still lets an operator tell "nobody is listening" from "I am being
//! rejected" — see [`tests::mqtt_bridge_distinguishes_a_wrong_key_from_an_unreachable_broker`].
//!
//! # System blocks are manifests, not modules
//!
//! `publisher` and `subscriber` (DAEMON §6.3, PROPOSED there and ratified by this being built)
//! are host-native: [`manifest_for`] builds their [`eio_manifest::Manifest`] in memory rather
//! than reading one out of a `.wasm`'s custom section, and [`crate::instance`] never compiles
//! anything for them. They are otherwise ordinary blocks — [`Manifest::validate`] accepts them,
//! [`crate::boot`] resolves a service's reference to one the same way it resolves any other, and
//! `GET /blocks` lists them so an agent or the Designer's palette finds them without knowing
//! they are special (SCOPE §4). The precedent stays narrow on purpose (CLAUDE.md invariant):
//! nothing else in this crate gets to skip the module.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use eio_manifest::{Abi, Manifest, Port, Property, PropertyType};
use eio_signal::{Batch, Value};
use tokio::sync::mpsc;

/// The topic namespace prefix every wire topic carries (DAEMON §7).
const NAMESPACE: &str = "eieio";

/// How many batches a subscriber's queue holds before a publish to it is dropped rather than
/// waited on.
///
/// SCOPE §3.4: "cross-device backpressure does not exist, because a publisher that cannot send
/// drops" — so this is a depth, not a promise; a subscriber slower than its publishers loses
/// batches rather than ever being waited for.
const SUBSCRIBER_CAPACITY: usize = 64;

/// The registry name of the publisher system block (DAEMON §6.3).
pub const PUBLISHER: &str = "publisher";
/// The registry name of the subscriber system block (DAEMON §6.3).
pub const SUBSCRIBER: &str = "subscriber";
/// The one property both system blocks carry: the topic they publish or subscribe to
/// (DAEMON §7).
const TOPIC_PROPERTY: &str = "topic";

/// Which system block an instance is, once its `block` reference has resolved to one
/// (DAEMON §6.3).
///
/// The precedent is deliberately this narrow set and no wider (CLAUDE.md invariant: "system
/// blocks are transport endpoints only").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemBlockKind {
    /// Publishes every batch delivered on `in` to its `topic` (no outputs).
    Publisher,
    /// Emits on `out` every batch published to its `topic` (no inputs).
    Subscriber,
}

impl SystemBlockKind {
    /// Which kind `name` (a resolved block reference's name, `eio_daemon::blocks::Entry::name`)
    /// names, or `None` for an ordinary block.
    ///
    /// Checked by name, the same thing a service file's `block` reference already resolves to
    /// (DAEMON §4) — a host-native block needs no version and no cache entry, so this is where
    /// [`crate::boot::resolve`] branches before either would be looked for.
    pub fn of(name: &str) -> Option<SystemBlockKind> {
        match name {
            PUBLISHER => Some(SystemBlockKind::Publisher),
            SUBSCRIBER => Some(SystemBlockKind::Subscriber),
            _ => None,
        }
    }

    /// The block's registry name — the inverse of [`of`](Self::of).
    pub fn name(self) -> &'static str {
        match self {
            SystemBlockKind::Publisher => PUBLISHER,
            SystemBlockKind::Subscriber => SUBSCRIBER,
        }
    }
}

/// The manifest a system block presents to the manifest system (ABI §11), built in memory
/// rather than read from a module (DAEMON §6.3).
///
/// `capabilities` is empty and always will be: a host-native block never imports anything,
/// there being no guest to import into. `targets` is `[]`, which is exactly what ABI §11.1
/// says an empty list means: no compiled artifact exists for this block, there being no
/// `.wasm` and no triple it was built for. Nothing refuses this manifest for it —
/// [`Manifest::validate`] accepts `[]` on the document alone — because nothing here ever
/// hands a host real module bytes to contradict it with; that refusal belongs to
/// [`eio_manifest::validate`], for a caller that did.
pub fn manifest_for(kind: SystemBlockKind) -> Manifest {
    let topic = Property {
        name: String::from(TOPIC_PROPERTY),
        ty: PropertyType::String,
        description: String::from(
            "The pub/sub topic this instance publishes or subscribes to, scoped under this \
             node's configured bus (DAEMON §7, §7.1): the wire topic is `eieio/<bus>/<topic>`. \
             Evaluated once, at configure time — it MUST be a signal-independent expression \
             (EXPR §6), because it names a destination for the instance's whole life rather \
             than a per-signal value.",
        ),
        default: None,
        required: true,
    };
    match kind {
        SystemBlockKind::Publisher => Manifest {
            name: String::from(kind.name()),
            version: String::from("1.0.0"),
            abi: Abi::CURRENT,
            description: String::from(
                "Publishes every batch delivered on `in` to this node's configured pub/sub \
                 bus (DAEMON §6.3, §7). Host-native: implemented by the router's bridge, not \
                 by WASM.",
            ),
            capabilities: Vec::new(),
            inputs: vec![Port {
                name: String::from("in"),
            }],
            outputs: Vec::new(),
            properties: vec![topic],
            // No compiled artifact exists for a host-native block (ABI §11.1) — see the
            // module docs above.
            targets: Vec::new(),
            aot: Vec::new(),
        },
        SystemBlockKind::Subscriber => Manifest {
            name: String::from(kind.name()),
            version: String::from("1.0.0"),
            abi: Abi::CURRENT,
            description: String::from(
                "Emits on `out` every batch published on this node's configured pub/sub bus \
                 on `topic` (DAEMON §6.3, §7). Host-native: implemented by the router's \
                 bridge, not by WASM.",
            ),
            capabilities: Vec::new(),
            inputs: Vec::new(),
            outputs: vec![Port {
                name: String::from("out"),
            }],
            properties: vec![topic],
            // No compiled artifact exists for a host-native block (ABI §11.1) — see the
            // module docs above.
            targets: Vec::new(),
            aot: Vec::new(),
        },
    }
}

/// Resolves and evaluates a system block's `topic` property, once, off any thread.
///
/// Reuses `eio_host_core::resolve` — the same required/default rule ABI §11.1 states for every
/// block — rather than restating it, so "the supplied expression, else the manifest's default,
/// else a configuration failure iff required" is one implementation for a WASM block's
/// properties and a system block's alike.
///
/// A system block's `topic` is evaluated once here instead of per signal like an ordinary
/// property (ABI §7.1): it names the instance's one destination for its whole life, not a
/// per-batch value, so it MUST be signal-independent — evaluating against no signal
/// (`eio_expr`'s `SIGNAL_NONE`) is what makes a signal-dependent expression a configuration
/// error here rather than a silent per-signal re-evaluation nothing above the bridge expects.
pub fn resolve_topic(
    manifest: &Manifest,
    supplied: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let sources = eio_host_core::resolve(manifest, supplied)
        .map_err(|error| anyhow::anyhow!("this configuration is invalid: {error}"))?;
    let source = sources
        .iter()
        .find(|source| source.name == TOPIC_PROPERTY)
        .expect("a system block's manifest always declares `topic`")
        .source
        .expect("`topic` is required, so `resolve` never leaves it unset");

    let value = eio_expr::eval_source(source, None).map_err(|error| {
        anyhow::anyhow!(
            "the `topic` property must be a signal-independent expression (EXPR §6): {error}"
        )
    })?;
    let Value::Str(topic) = value else {
        anyhow::bail!("the `topic` property must evaluate to a string, not {value:?}");
    };
    if !eio_manifest::is_ref_name(&topic) {
        anyhow::bail!(
            "\"{topic}\" is not a valid topic: a topic follows ABI §11.1's name pattern \
             (DAEMON §7), the same as a block's own name"
        );
    }
    Ok(topic)
}

/// The full wire topic a `topic` property resolves to (DAEMON §7): `eieio/<bus>/<topic>`.
///
/// `bus` comes from this node's `pubsub.toml` (`crate::pubsub::Pubsub::bus`, DAEMON §7.1) —
/// **not** this node's System: DAEMON §10 states a node does not know its System, and a bus
/// is a different thing, an address a node is *given* when it is told to join one. `bus` is
/// never itself part of a block's configuration, the same way it is never a block property.
pub fn wire_topic(bus: &str, topic: &str) -> String {
    format!("{NAMESPACE}/{bus}/{topic}")
}

/// The bus [`crate::executor::Executor::build`] answers with before anything wires a real one
/// in ([`crate::executor::Executor::bridging`]).
///
/// Never actually reaches a topic that goes anywhere: the executor's default bridge is
/// [`InProcessBridge::disconnected`], so every publish already drops regardless of this
/// string. It exists so [`wire_topic`] always has *something* to format rather than an
/// `Option` every caller has to unwrap for a value nothing downstream will use.
pub(crate) const UNCONFIGURED_BUS: &str = "unconfigured";

/// What became of one [`Bridge::publish`] call (SCOPE §3.4).
///
/// Not an error: dropping is the *normal* answer to a bridge that cannot send right now, which
/// is what "cross-device backpressure does not exist" means. A caller logs and counts a
/// [`Dropped`](Delivery::Dropped) (DAEMON §6.2) and moves on; there is nothing to retry, because
/// retrying would be this crate inventing the stronger guarantee SCOPE §3.4 declines to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Handed to the transport. At-most-once still permits this to be lost in flight
    /// (SCOPE §3.4) — a real transport's part of the guarantee, not this return value's.
    Sent,
    /// Not handed to the transport at all: there was no connection to send it on.
    Dropped,
}

/// The receiving half of one [`Bridge::subscribe`] call.
///
/// A batch at a time, in publish order per publisher (SCOPE §3.4 orders each publisher's own
/// stream and promises nothing between two publishers on one topic). Ends — `recv` answers
/// `None` — when the bridge itself is gone; an instance built around a [`Subscription`] treats
/// that the same as any other reason its upstream stopped.
#[derive(Debug)]
pub struct Subscription {
    rx: mpsc::Receiver<Batch>,
}

impl Subscription {
    /// The next batch published on this subscription's topic, or `None` once the bridge is
    /// gone.
    pub async fn recv(&mut self) -> Option<Batch> {
        self.rx.recv().await
    }
}

/// The bridge trait (DAEMON §7): publish, subscribe, and a connection's lifecycle — and
/// **nothing else**. See the module docs for what that boundary buys and how it is checked.
pub trait Bridge: Send + Sync {
    /// Publishes `batch` on `topic`, without blocking (SCOPE §3.4: no cross-device
    /// backpressure). `topic` is already the full wire topic ([`wire_topic`]) — the bridge
    /// itself has no opinion about what a bus is, only what to do with a string.
    fn publish(&self, topic: &str, batch: Batch) -> Delivery;

    /// Subscribes to `topic`, receiving what is published on it from this call forward. No
    /// retained messages (DAEMON §7): a subscription started after a publish never sees it,
    /// the same as a fresh mailbox in `crate::router` carries no history.
    fn subscribe(&self, topic: &str) -> Subscription;

    /// Whether this bridge currently has a connection it can publish or subscribe through.
    ///
    /// The lifecycle half of the trait. A `false` answer is not an error — a bridge that has
    /// not connected yet, or has lost its connection, is a bridge every [`publish`](Bridge::publish)
    /// on it answers [`Delivery::Dropped`] (SCOPE §3.4's "a publisher that cannot send drops"),
    /// which is exactly the loss path this crate's tests prove rather than assume.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no production call site yet: nothing on the daemon's own path branches \
                      on connectivity today, only the tests that force the loss path"
        )
    )]
    fn is_connected(&self) -> bool;
}

/// One topic's current subscribers, inside a [`Broker`].
type Subscribers = HashMap<String, Vec<mpsc::Sender<Batch>>>;

/// The shared state behind every [`InProcessBridge`] connected to it — this crate's stand-in
/// for an MQTT broker (DAEMON §7).
///
/// Two [`InProcessBridge`]s from one `Broker` are what "two daemons on one broker" (the
/// acceptance criterion) means for the in-process transport: each represents one node's
/// connection, and what flows between them never touches the other's process because there is
/// only one process — the point being that nothing above [`Bridge`] can tell the difference.
#[derive(Debug, Default)]
pub struct Broker {
    subscribers: std::sync::Mutex<Subscribers>,
    /// Every publish this broker could not hand to a subscriber, for whatever reason
    /// (SCOPE §3.4, DAEMON §6.2: "every loss is logged and counted at the bridge").
    dropped: AtomicU64,
}

impl Broker {
    /// A fresh broker with nobody connected to it yet.
    pub fn new() -> Arc<Broker> {
        Arc::new(Broker::default())
    }

    /// A connection to this broker — one [`InProcessBridge`] per node, in the tests that use
    /// this transport.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a production `Broker` needs a caller that elects one broker across \
                      several nodes (DAEMON §7's expansion item); today's only caller is the \
                      in-process tests, which stand every node up in one process"
        )
    )]
    pub fn connect(self: &Arc<Broker>) -> InProcessBridge {
        InProcessBridge {
            broker: Arc::clone(self),
            connected: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Every publish this broker has dropped since it was created, across every connection.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no metrics endpoint reads this yet; today's only caller is a test \
                      asserting the loss path"
        )
    )]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }
}

/// The in-process test transport (module docs): a [`Bridge`] backed by a [`Broker`] shared
/// with every other connection to it, and nothing else — no socket, no thread of its own, no
/// network in any test that uses it.
#[derive(Debug, Clone)]
pub struct InProcessBridge {
    broker: Arc<Broker>,
    connected: Arc<AtomicBool>,
}

impl InProcessBridge {
    /// A bridge with no broker behind it at all — what an [`crate::executor::Executor`] is
    /// given when nothing has wired a real one in.
    ///
    /// Every publish on this drops (`is_connected` is `false` from construction), which is the
    /// honest answer until a transport is actually configured: `publisher`/`subscriber` stay
    /// discoverable and loadable (DAEMON §6.3) on a node with no pub/sub wired up, and every
    /// batch handed to one is counted rather than silently swallowed or, worse, buffered
    /// somewhere nothing will ever drain.
    pub fn disconnected() -> InProcessBridge {
        InProcessBridge {
            broker: Broker::new(),
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Severs this connection, as if the transport under it had dropped — a test's way of
    /// forcing SCOPE §3.4's loss path without a network to actually sever.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "nothing in this crate reconnects or disconnects a bridge yet — a real \
                      transport's own connection lifecycle is eieio-2vm's follow-up. Today's \
                      only caller is a test forcing the loss path deterministically"
        )
    )]
    pub fn disconnect(&self) {
        self.connected.store(false, Ordering::SeqCst);
    }

    /// Every publish dropped on this connection's broker since it was created.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no metrics endpoint reads this yet; today's only caller is a test \
                      asserting the loss path"
        )
    )]
    pub fn dropped(&self) -> u64 {
        self.broker.dropped()
    }
}

impl Bridge for InProcessBridge {
    fn publish(&self, topic: &str, batch: Batch) -> Delivery {
        if !self.connected.load(Ordering::SeqCst) {
            self.broker.dropped.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(topic, "dropped a publish: this bridge has no connection");
            return Delivery::Dropped;
        }

        let mut subscribers = self
            .broker
            .subscribers
            .lock()
            .expect("the broker's lock is never held across a panic");
        if let Some(senders) = subscribers.get_mut(topic) {
            // Bounded, and never waited on (SCOPE §3.4): a subscriber behind on its own queue
            // loses this batch rather than slowing the publisher down, which is what "cross-
            // device backpressure does not exist" means from the sending side. A closed
            // receiver — the subscribing instance is gone — is dropped from the list rather
            // than counted: nothing was lost, because nothing was listening any more.
            senders.retain_mut(|sender| match sender.try_send(batch.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.broker.dropped.fetch_add(1, Ordering::SeqCst);
                    tracing::warn!(topic, "dropped a publish: a subscriber's queue is full");
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
        }
        Delivery::Sent
    }

    fn subscribe(&self, topic: &str) -> Subscription {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        self.broker
            .subscribers
            .lock()
            .expect("the broker's lock is never held across a panic")
            .entry(String::from(topic))
            .or_default()
            .push(tx);
        Subscription { rx }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

/// The QoS this transport maps SCOPE §3.4's at-most-once onto (DAEMON §7). Stated once, as a
/// constant never read from configuration: "the mapping belongs here and nowhere above", and a
/// mapping with one entry needs no knob.
const QOS: rumqttc::QoS = rumqttc::QoS::AtMostOnce;

/// The MQTT username every connection presents when `pubsub.toml` carries a `key`.
///
/// A fixed, non-secret constant rather than anything derived from this node — the pre-shared
/// key is scoped to a bus, not a node (SCOPE §3.11 states plainly that this scheme carries no
/// per-node identity), and MQTT's own CONNECT packet has no way to send a password without a
/// username alongside it. Every connection to a keyed bus presents this same value; only the
/// password half (the key itself) is checked.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read only by `dial`; see `MqttBridge::connect`'s own `expect`"
    )
)]
const MQTT_USERNAME: &str = "eieio";

/// How many outstanding requests (a publish or a subscribe not yet handed to the socket)
/// `rumqttc`'s internal channel holds before [`MqttBridge::publish`] and
/// [`MqttBridge::subscribe`] refuse rather than wait — the same non-blocking posture
/// [`SUBSCRIBER_CAPACITY`] gives the in-process transport, for the same reason (SCOPE §3.4: "a
/// publisher that cannot send drops"). Small on purpose: this is a depth against a momentary
/// stall in the client's own event loop, not a durable queue, and a `Bridge::publish` that could
/// still be "sent" long after the caller moved on would be this crate inventing the stronger
/// guarantee SCOPE §3.4 declines to make.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read only by `dial`, which is read only by `run`, which is read only by \
                  `MqttBridge::connect` — see that function's own `expect` for why nothing on \
                  the daemon's production path calls it yet"
    )
)]
const REQUEST_CAPACITY: usize = 16;

/// How long [`MqttBridge::connect`]'s ranked walk waits for one candidate to answer before
/// trying the next (DAEMON §7.1: "connects as an ordinary client"). Not a normative value —
/// DAEMON §7.1 states the walk, not a duration — chosen short because a candidate that is going
/// to accept a plain loopback-speed TCP connection does so in milliseconds, and a walk that
/// lingers on an unreachable rank-1 candidate delays every candidate behind it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read only by `dial`; see `MqttBridge::connect`'s own `expect`"
    )
)]
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// How long [`MqttBridge::connect`]'s background task waits before repeating the whole ranked
/// walk when nothing in it answered (DAEMON §7.1: "retries with backoff while its publishes
/// drop"). Also not a normative value, for the same reason as [`DIAL_TIMEOUT`].
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read only by `run`; see `MqttBridge::connect`'s own `expect`"
    )
)]
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// One entry from `pubsub.toml`'s ranked `candidates` list (DAEMON §7.1): `<node-id>@<host>:<port>`.
///
/// Deliberately holds no MQTT vocabulary — `id`, a host and a port are what *any* transport
/// would need to dial an address in rank order, which is the property the boundary asks for:
/// nothing here is a reason this type could not sit above [`Bridge`] instead of behind it.  It
/// stays in this module anyway, because parsing and walking a candidate list is exactly the
/// "real transport's job" `crate::pubsub`'s own docs already deferred to this issue.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "constructed only by a caller that has parsed `pubsub.toml`'s `candidates`; \
                  no production call site exists yet — see `MqttBridge::connect`'s own `expect`"
    )
)]
pub struct Candidate {
    /// The candidate's own node id — what `pinned` in `pubsub.toml` names (DAEMON §7.1).
    pub id: String,
    /// The host or IP a client dials to reach this candidate.
    pub host: String,
    /// The port a client dials to reach this candidate.
    pub port: u16,
}

impl Candidate {
    /// Parses one `pubsub.toml` `candidates` entry: `<id>@<host>:<port>`, DAEMON §7.1's own
    /// example (`n7k2p4qv@10.0.0.5:1883`) verbatim. `<host>` is taken up to the *last* `:`, so
    /// an IPv6 literal would need bracket syntax this parser does not yet accept — nothing in
    /// DAEMON §7.1's example or SCOPE §3.9's static-discovery posture asks for one yet, so that
    /// is a gap to close when an address shaped like one actually shows up, not a case to guess
    /// at here.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no production caller parses a raw `candidates` entry yet — see \
                      `MqttBridge::connect`'s own `expect`; today's only caller is this \
                      module's own parsing tests"
        )
    )]
    pub fn parse(raw: &str) -> anyhow::Result<Candidate> {
        let (id, address) = raw.split_once('@').ok_or_else(|| {
            anyhow::anyhow!("\"{raw}\" is not `<id>@<host>:<port>` (DAEMON §7.1): no `@`")
        })?;
        let (host, port) = address.rsplit_once(':').ok_or_else(|| {
            anyhow::anyhow!("\"{raw}\" is not `<id>@<host>:<port>` (DAEMON §7.1): no `:`")
        })?;
        anyhow::ensure!(!id.is_empty(), "\"{raw}\" names no id before `@`");
        anyhow::ensure!(
            !host.is_empty(),
            "\"{raw}\" names no host between `@` and `:`"
        );
        let port: u16 = port
            .parse()
            .map_err(|error| anyhow::anyhow!("\"{raw}\": \"{port}\" is not a port: {error}"))?;
        Ok(Candidate {
            id: String::from(id),
            host: String::from(host),
            port,
        })
    }
}

/// One topic's current subscribers on an [`MqttBridge`] — the same shape as [`Subscribers`],
/// kept as its own alias so a change to one transport's fan-out never silently reaches for the
/// other's type.
type MqttSubscribers = Mutex<HashMap<String, Vec<mpsc::Sender<Batch>>>>;

/// The MQTT transport (module docs, DAEMON §7): [`Bridge`] over `rumqttc`, DAEMON §7.1's
/// ranked-candidate walk, QoS 0, never retained.
///
/// Every field is shared with the background task [`MqttBridge::connect`] spawns: this handle's
/// [`publish`](Bridge::publish) and [`subscribe`](Bridge::subscribe) are synchronous and never
/// touch the network directly, so they read whatever the task last published to these — the
/// same non-blocking shape [`InProcessBridge`] already has, over a real socket instead of a
/// broker in this process.
#[derive(Debug, Clone)]
pub struct MqttBridge {
    /// The live client, if the background task currently has a broker connection. `None` is
    /// exactly [`InProcessBridge::disconnected`]'s state for this transport: nothing to publish
    /// or subscribe onto, so every publish drops.
    client: Arc<Mutex<Option<rumqttc::AsyncClient>>>,
    /// Mirrors whether `client` is currently `Some`, as an atomic so [`Bridge::is_connected`]
    /// and the hot path of [`Bridge::publish`] need no lock to answer.
    connected: Arc<AtomicBool>,
    /// Every topic an instance has asked to hear, and where to deliver it — reinstated against
    /// a fresh client after every reconnect, because a clean MQTT session remembers no
    /// subscription across one (module docs: "handover has nothing to hand over").
    subscribers: Arc<MqttSubscribers>,
    /// Every publish this bridge could not hand to the client, whether because nothing was
    /// connected or because the client's own request channel was full (DAEMON §6.2).
    dropped: Arc<AtomicU64>,
    /// Every candidate that answered and then refused the connection during the handshake
    /// (module docs, SCOPE §3.11, eieio-2vm.5) — kept apart from `dropped`, which counts a
    /// publish lost *after* connecting, so the two can never be confused: a bridge that has
    /// never connected has `dropped() == 0` regardless of why, and `rejected()` is the count
    /// that tells an operator "somebody answered and said no" rather than "nobody answered".
    rejected: Arc<AtomicU64>,
}

impl MqttBridge {
    /// Starts the background connection task and returns a handle to it immediately —
    /// disconnected until the task's first successful dial, exactly like
    /// [`InProcessBridge::disconnected`] until then.
    ///
    /// Runs DAEMON §7.1's walk forever, on its own OS thread with its own current-thread tokio
    /// runtime (so a caller need not already be inside one): with `pinned` unset, it dials
    /// `candidates` in rank order and stays on the first that accepts a full MQTT connection;
    /// with `pinned` set, it dials only the candidate whose `id` matches and never considers the
    /// rest, honouring §7.1's "while it is present, candidates do not self-promote". Losing a
    /// connection — however it was chosen — restarts the same walk from the top, which is
    /// deliberately the same code path as the first connect (module docs: "handover has nothing
    /// to hand over").
    ///
    /// **What this does not do, by this issue's scope:** when nothing in the walk answers, DAEMON
    /// §7.1 says "this node would host one" — hosting a broker is out of scope here, so this
    /// task instead does what §7.1 already calls the ordinary case of "nothing reachable": retry
    /// the walk on [`RETRY_INTERVAL`] while every [`Bridge::publish`] on this handle drops,
    /// logged and counted like any other discard (DAEMON §6.2). A future that does implement
    /// hosting adds a branch here; it does not change this method's signature or its callers.
    ///
    /// `key`, when given, is presented as this connection's MQTT credential (module docs, SCOPE
    /// §3.11, eieio-2vm.5) — `None` is the pre-existing behaviour and remains supported exactly
    /// as before: a bus with no key presents none, and a candidate with none configured accepts
    /// that.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no production caller yet: wiring this into a running node is `main.rs`'s \
                      one-line swap of `InProcessBridge::disconnected` (module docs), and \
                      `main.rs` is outside this change's file-ownership boundary. Today's only \
                      caller is this module's own tests"
        )
    )]
    pub fn connect(
        client_id: String,
        candidates: Vec<Candidate>,
        pinned: Option<String>,
        key: Option<crate::pubsub::Key>,
    ) -> MqttBridge {
        let bridge = MqttBridge {
            client: Arc::new(Mutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            dropped: Arc::new(AtomicU64::new(0)),
            rejected: Arc::new(AtomicU64::new(0)),
        };
        let task = bridge.clone();

        std::thread::Builder::new()
            .name(String::from("eio-mqtt-bridge"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("building the bridge task's own tokio runtime");
                runtime.block_on(run(client_id, candidates, pinned, key, task));
            })
            .expect("spawning the bridge's connection thread");

        bridge
    }

    /// Every publish this bridge has dropped since it was created — the real-transport
    /// equivalent of [`InProcessBridge::dropped`], read by the same kind of test.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no metrics endpoint reads this yet; today's only caller is a test \
                      asserting the loss path"
        )
    )]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }

    /// Every candidate that has answered and then refused this bridge's connection during the
    /// handshake since it was created — the "I am being rejected" half of SCOPE §3.11's
    /// acceptance criterion, kept apart from [`Self::dropped`]'s "nobody is listening" half (see
    /// [`DialError`]'s docs for how the two are told apart).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no metrics endpoint reads this yet; today's only caller is a test \
                      asserting the rejection path"
        )
    )]
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }
}

impl Bridge for MqttBridge {
    fn publish(&self, topic: &str, batch: Batch) -> Delivery {
        if !self.connected.load(Ordering::SeqCst) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(topic, "dropped a publish: no broker connection");
            return Delivery::Dropped;
        }

        let sent = {
            let guard = self
                .client
                .lock()
                .expect("the bridge's lock is never held across a panic");
            match guard.as_ref() {
                // QoS 0, never retained (DAEMON §7): the two are stated once, here, as the
                // literal arguments rather than anything read from configuration.
                Some(client) => client
                    .try_publish(topic, QOS, false, batch.to_cbor())
                    .is_ok(),
                None => false,
            }
        };
        if sent {
            Delivery::Sent
        } else {
            // Either `connected` was stale (the task disconnected between the check above and
            // this lock) or `rumqttc`'s own request channel is full — either way, this publish
            // was not hand to the client, so it is dropped and counted exactly like the
            // disconnected case (DAEMON §6.2).
            self.dropped.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                topic,
                "dropped a publish: could not hand it to the mqtt client"
            );
            Delivery::Dropped
        }
    }

    fn subscribe(&self, topic: &str) -> Subscription {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        self.subscribers
            .lock()
            .expect("the bridge's lock is never held across a panic")
            .entry(String::from(topic))
            .or_default()
            .push(tx);
        // Best-effort: if nothing is connected yet, `run`'s reconnect path resubscribes every
        // recorded topic once it dials successfully (see its docs), so this call never needs to
        // retry on its own.
        if let Some(client) = self
            .client
            .lock()
            .expect("the bridge's lock is never held across a panic")
            .as_ref()
        {
            let _ = client.try_subscribe(topic, QOS);
        }
        Subscription { rx }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

/// Delivers every inbound `Publish` this connection receives to whichever subscribers are
/// currently registered for its topic, decoding the wire bytes back into a [`Batch`]
/// (`Batch::to_cbor`/`from_cbor`, ABI §6.3.1) — the same canonical form every transport carries.
///
/// A payload that will not decode is logged and dropped rather than panicking: once this
/// crate's own daemons are not the only possible publisher on a bus (SCOPE §3.9's shared-broker
/// posture), a malformed payload is a fact about the wire, not a bug in this process.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "called only from `drive`, which runs only inside `MqttBridge::connect`'s \
                  background task — see that function's own `expect`"
    )
)]
fn dispatch(topic: &str, payload: &[u8], subscribers: &MqttSubscribers, dropped: &AtomicU64) {
    let batch = match Batch::from_cbor(payload) {
        Ok(batch) => batch,
        Err(error) => {
            dropped.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(topic, %error, "dropped an inbound publish: not a valid batch");
            return;
        }
    };
    let mut subscribers = subscribers
        .lock()
        .expect("the bridge's lock is never held across a panic");
    if let Some(senders) = subscribers.get_mut(topic) {
        // The same bounded, never-waited-on fan-out as `InProcessBridge::publish` (SCOPE §3.4):
        // a subscriber behind on its own queue loses this batch rather than stalling delivery
        // to every other subscriber of this connection.
        senders.retain_mut(|sender| match sender.try_send(batch.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(
                    topic,
                    "dropped an inbound publish: a subscriber's queue is full"
                );
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        });
    }
}

/// What went wrong dialing one candidate — the shape [`dial`] classifies every failure into, so
/// [`run`] can tell DAEMON §7.1's ordinary "nothing reachable" apart from a wrong or missing bus
/// key (module docs, SCOPE §3.11, eieio-2vm.5) rather than logging and counting both alike.
///
/// The classification is a network-level heuristic, not a protocol guarantee stated anywhere:
/// [`tests::spawn_embedded_broker`]'s auth check never sends a failure `ConnAck` at all, per
/// `rumqttd` 0.20's own implementation — it closes the socket having read the `Connect` packet
/// and rejected it. A TCP handshake that completes and is then closed before a `ConnAck`
/// arrives is the strongest signal available that *somebody*, not nobody, is on the other end,
/// and it is also the shape a broker crashing mid-handshake would take — this type does not
/// claim to know which. What it does claim, and what
/// [`tests::mqtt_bridge_distinguishes_a_wrong_key_from_an_unreachable_broker`] proves, is that
/// this shape is never produced by a candidate nothing is listening on.
#[derive(Debug)]
enum DialError {
    /// No usable connection at the transport level: refused, timed out, or the far end was
    /// never reached at all. DAEMON §7.1's ordinary "nothing reachable".
    Unreachable(anyhow::Error),
    /// A broker was there, and the connection ended during the handshake rather than
    /// completing it — the shape a wrong or missing key takes on the wire.
    Rejected(anyhow::Error),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::Unreachable(error) | DialError::Rejected(error) => write!(f, "{error}"),
        }
    }
}

/// Attempts one full MQTT connection to `candidate` — DAEMON §7.1's "connects as an ordinary
/// client" — bounded by [`DIAL_TIMEOUT`]. `Ok` means `candidate` answered and is now the broker
/// this call sees; `Err` classifies everything else, per [`DialError`]'s own docs.
///
/// `key`, when given, is presented as this connection's MQTT credential under
/// [`MQTT_USERNAME`] — never logged, never placed in an error message: every error this
/// function builds names the failure shape, not the credential that produced it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "called only from `run`, which runs only inside `MqttBridge::connect`'s \
                  background task — see that function's own `expect`"
    )
)]
async fn dial(
    client_id: &str,
    candidate: &Candidate,
    key: Option<&crate::pubsub::Key>,
) -> Result<(rumqttc::AsyncClient, rumqttc::EventLoop), DialError> {
    let mut options = rumqttc::MqttOptions::new(client_id, candidate.host.clone(), candidate.port);
    options.set_keep_alive(Duration::from_secs(30));
    if let Some(key) = key {
        options.set_credentials(MQTT_USERNAME, key.expose());
    }
    let (client, mut eventloop) = rumqttc::AsyncClient::new(options, REQUEST_CAPACITY);
    eventloop
        .network_options
        .set_connection_timeout(DIAL_TIMEOUT.as_secs());

    match tokio::time::timeout(DIAL_TIMEOUT, eventloop.poll()).await {
        Ok(Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(ack))))
            if ack.code == rumqttc::ConnectReturnCode::Success =>
        {
            Ok((client, eventloop))
        }
        // A `ConnAck` that arrives but says no is unambiguous: a broker answered and refused.
        // `rumqttc`'s own connect handshake in fact turns this into `ConnectionRefused` below
        // before this arm is ever reached (it never surfaces a failing `ConnAck` as an
        // `Incoming` event) — this arm stays as the direct, if currently unreachable, reading
        // of what a failing `ConnAck` means, in case a future `rumqttc` changes that.
        Ok(Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(ack)))) => {
            Err(DialError::Rejected(anyhow::anyhow!(
                "the broker refused the connection: {:?}",
                ack.code
            )))
        }
        Ok(Ok(other)) => Err(DialError::Unreachable(anyhow::anyhow!(
            "expected a ConnAck, got {other:?}"
        ))),
        // The compliant-broker case: a `ConnAck` with a failing code, surfaced by `rumqttc`'s
        // own connect handshake as this variant rather than as an `Incoming` event (see above).
        Ok(Err(rumqttc::ConnectionError::ConnectionRefused(code))) => Err(DialError::Rejected(
            anyhow::anyhow!("the broker refused the connection: {code:?}"),
        )),
        // The fixture broker's case (this function's own docs): no `ConnAck` at all, just a
        // socket closed mid-handshake. Narrowed to this one `StateError` variant rather than
        // every `MqttState` error, because `dial`'s event loop has done nothing yet but this
        // one handshake — nothing else here could produce the state machine's other errors.
        Ok(Err(rumqttc::ConnectionError::MqttState(rumqttc::StateError::ConnectionAborted))) => {
            Err(DialError::Rejected(anyhow::anyhow!(
                "the broker closed the connection during the handshake"
            )))
        }
        Ok(Err(error)) => Err(DialError::Unreachable(error.into())),
        Err(_) => Err(DialError::Unreachable(anyhow::anyhow!(
            "no answer within {DIAL_TIMEOUT:?}"
        ))),
    }
}

/// Polls one live connection until it ends, dispatching every inbound publish to `subscribers`
/// along the way. Returns — never with an error a caller need inspect — the moment the
/// connection is no longer usable, so [`run`] can restart DAEMON §7.1's walk from the top.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "called only from `run`, which runs only inside `MqttBridge::connect`'s \
                  background task — see that function's own `expect`"
    )
)]
async fn drive(
    mut eventloop: rumqttc::EventLoop,
    subscribers: &MqttSubscribers,
    dropped: &AtomicU64,
) {
    loop {
        match eventloop.poll().await {
            Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(publish))) => {
                dispatch(&publish.topic, &publish.payload, subscribers, dropped);
            }
            Ok(rumqttc::Event::Incoming(rumqttc::Packet::Disconnect)) => return,
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(%error, "the mqtt connection ended");
                return;
            }
        }
    }
}

/// [`MqttBridge::connect`]'s background task: DAEMON §7.1's walk, forever.
///
/// `pinned` is applied once, up front, as a filter over `candidates` rather than a branch taken
/// per attempt — "while it is present, candidates do not self-promote" holds automatically if
/// the only candidate this loop ever sees is the pinned one.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "spawned only by `MqttBridge::connect` — see that function's own `expect`"
    )
)]
async fn run(
    client_id: String,
    candidates: Vec<Candidate>,
    pinned: Option<String>,
    key: Option<crate::pubsub::Key>,
    // The five fields `MqttBridge::connect` already built its handle from, taken as the handle
    // itself rather than five separate `Arc`s — a `MqttBridge` is exactly this task's shared
    // state, and bundling it here is what keeps this signature under clippy's argument count
    // rather than adding an `#[allow]` every future field would have to remember to extend.
    bridge: MqttBridge,
) {
    let MqttBridge {
        client,
        connected,
        subscribers,
        dropped,
        rejected,
    } = bridge;
    let candidates: Vec<Candidate> = match &pinned {
        Some(id) => candidates.into_iter().filter(|c| &c.id == id).collect(),
        None => candidates,
    };
    if candidates.is_empty() {
        tracing::warn!(
            pinned = pinned.as_deref(),
            "no dialable candidate (an empty list, or a pin naming none of them): every \
             publish on this bridge will drop until `pubsub.toml` changes"
        );
        return;
    }

    loop {
        let mut answered = false;
        for candidate in &candidates {
            let (new_client, eventloop) = match dial(&client_id, candidate, key.as_ref()).await {
                Ok(pair) => pair,
                Err(DialError::Unreachable(error)) => {
                    tracing::debug!(candidate = %candidate.id, %error, "candidate did not answer");
                    continue;
                }
                Err(DialError::Rejected(error)) => {
                    // Never a `dropped` publish (module docs' distinction) — nothing was ever
                    // handed to a connection, because there never was one. Counted and logged
                    // apart from the ordinary "nothing reachable" case so an operator watching
                    // this bridge can tell them apart (SCOPE §3.11, eieio-2vm.5's acceptance
                    // criterion).
                    rejected.fetch_add(1, Ordering::SeqCst);
                    tracing::warn!(
                        candidate = %candidate.id,
                        %error,
                        "candidate refused the connection during the handshake: check this \
                         bus's key in pubsub.toml"
                    );
                    continue;
                }
            };
            answered = true;
            tracing::info!(candidate = %candidate.id, "connected to the pub/sub broker");

            // A fresh session remembers no subscription (module docs): reinstate every topic
            // an instance is currently waiting on before this connection starts fanning batches
            // out, so a subscriber that existed before this (re)connect never misses a publish
            // that arrives right after it.
            for topic in subscribers
                .lock()
                .expect("the bridge's lock is never held across a panic")
                .keys()
            {
                let _ = new_client.try_subscribe(topic.clone(), QOS);
            }

            *client
                .lock()
                .expect("the bridge's lock is never held across a panic") = Some(new_client);
            connected.store(true, Ordering::SeqCst);

            drive(eventloop, &subscribers, &dropped).await;

            connected.store(false, Ordering::SeqCst);
            *client
                .lock()
                .expect("the bridge's lock is never held across a panic") = None;
            // Handover has nothing to hand over (module docs): restart the walk from rank 1
            // rather than falling through to the next candidate, so a higher-ranked candidate
            // that comes back is preferred again immediately.
            break;
        }
        if !answered {
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use eio_signal::Signal;

    fn batch(n: i64) -> Batch {
        let mut signal = Signal::new();
        signal.set("n", Value::Int(n));
        let mut batch = Batch::new();
        batch.push(signal);
        batch
    }

    fn n_of(batch: &Batch) -> i64 {
        match batch.get(0).and_then(|signal| signal.get("n")) {
            Some(Value::Int(n)) => *n,
            other => panic!("expected an int, got {other:?}"),
        }
    }

    #[test]
    fn the_system_blocks_are_valid_manifests() {
        // The manifest system's own gate (ABI §11.1), because a document nobody would accept
        // from a `.wasm` should not be accepted here either just for lacking one.
        for kind in [SystemBlockKind::Publisher, SystemBlockKind::Subscriber] {
            let manifest = manifest_for(kind);
            manifest
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error}", kind.name()));
            assert_eq!(SystemBlockKind::of(kind.name()), Some(kind));
        }
    }

    #[test]
    fn an_ordinary_block_name_is_not_a_system_block() {
        assert_eq!(SystemBlockKind::of("transform"), None);
        assert_eq!(SystemBlockKind::of("filter"), None);
    }

    #[test]
    fn wire_topic_is_the_namespace_the_bus_and_the_topic() {
        assert_eq!(
            wire_topic("greenhouse", "temperature"),
            "eieio/greenhouse/temperature"
        );
    }

    #[test]
    fn resolve_topic_reads_the_supplied_expression() {
        let manifest = manifest_for(SystemBlockKind::Publisher);
        let mut supplied = BTreeMap::new();
        supplied.insert(String::from("topic"), String::from("\"temperature\""));
        assert_eq!(resolve_topic(&manifest, &supplied).unwrap(), "temperature");
    }

    #[test]
    fn resolve_topic_refuses_a_missing_required_property() {
        let manifest = manifest_for(SystemBlockKind::Publisher);
        let error = resolve_topic(&manifest, &BTreeMap::new()).unwrap_err();
        assert!(
            error.to_string().contains("invalid"),
            "expected a configuration error, got {error}"
        );
    }

    #[test]
    fn resolve_topic_refuses_a_signal_dependent_expression() {
        // `topic` is evaluated once, against no signal at all (EXPR's SIGNAL_NONE): a `$field`
        // reference has nothing to read and fails, which is the point.
        let manifest = manifest_for(SystemBlockKind::Publisher);
        let mut supplied = BTreeMap::new();
        supplied.insert(String::from("topic"), String::from("$sensor"));
        let error = resolve_topic(&manifest, &supplied).unwrap_err();
        assert!(
            error.to_string().contains("signal-independent"),
            "expected the signal-dependence refusal, got {error}"
        );
    }

    #[test]
    fn resolve_topic_refuses_a_non_string_value() {
        let manifest = manifest_for(SystemBlockKind::Publisher);
        let mut supplied = BTreeMap::new();
        supplied.insert(String::from("topic"), String::from("42"));
        let error = resolve_topic(&manifest, &supplied).unwrap_err();
        assert!(error.to_string().contains("must evaluate to a string"));
    }

    #[test]
    fn resolve_topic_refuses_a_name_that_is_not_a_valid_topic() {
        let manifest = manifest_for(SystemBlockKind::Publisher);
        let mut supplied = BTreeMap::new();
        supplied.insert(String::from("topic"), String::from("\"Not A Topic!\""));
        let error = resolve_topic(&manifest, &supplied).unwrap_err();
        assert!(error.to_string().contains("not a valid topic"));
    }

    #[tokio::test]
    async fn a_publish_reaches_every_current_subscriber() {
        let broker = Broker::new();
        let publisher = broker.connect();
        let mut a = publisher.subscribe("eieio/sys/t");
        let mut b = publisher.subscribe("eieio/sys/t");

        assert_eq!(publisher.publish("eieio/sys/t", batch(1)), Delivery::Sent);
        assert_eq!(n_of(&a.recv().await.expect("subscriber a got it")), 1);
        assert_eq!(n_of(&b.recv().await.expect("subscriber b got it")), 1);
    }

    #[tokio::test]
    async fn a_subscription_never_sees_what_was_published_before_it_existed() {
        // No retained messages (DAEMON §7): late is late, the same as a fresh in-node mailbox
        // carries no history.
        let broker = Broker::new();
        let publisher = broker.connect();
        publisher.publish("eieio/sys/t", batch(1));

        let mut late = publisher.subscribe("eieio/sys/t");
        publisher.publish("eieio/sys/t", batch(2));
        assert_eq!(n_of(&late.recv().await.expect("only the second")), 2);
    }

    #[tokio::test]
    async fn publishing_with_no_subscribers_is_not_a_loss() {
        // An unwired output is an ordinary shape and says nothing (ABI §6.4's own rule,
        // applied here): nobody having subscribed yet is not the same as a bridge that could
        // not send.
        let broker = Broker::new();
        let publisher = broker.connect();
        assert_eq!(publisher.publish("eieio/sys/t", batch(1)), Delivery::Sent);
        assert_eq!(broker.dropped(), 0);
    }

    /// Proves SCOPE §3.4's loss path: a publisher that cannot send drops, and the drop is
    /// logged and counted rather than assumed. Disconnected is the plainest way a bridge
    /// "cannot send" — no live connection to hand the batch to at all.
    #[tokio::test]
    async fn a_disconnected_bridge_drops_and_counts_rather_than_sending() {
        let bridge = InProcessBridge::disconnected();
        let mut never = bridge.subscribe("eieio/sys/t");

        assert_eq!(bridge.publish("eieio/sys/t", batch(1)), Delivery::Dropped);
        assert_eq!(bridge.dropped(), 1, "the drop was counted");
        assert!(
            never.rx.try_recv().is_err(),
            "a dropped publish never reaches a subscriber"
        );

        // And it keeps happening, not just once: every publish on a bridge with nothing behind
        // it drops, because there is nothing that would ever make it connected.
        assert_eq!(bridge.publish("eieio/sys/t", batch(2)), Delivery::Dropped);
        assert_eq!(bridge.dropped(), 2);
    }

    /// The other way a bridge "cannot send": at-most-once permits loss, and a subscriber that
    /// is not draining is exactly where SCOPE §3.4 says a batch may be lost rather than
    /// buffered without bound or waited for.
    #[tokio::test]
    async fn a_full_subscriber_drops_without_blocking_the_publisher() {
        let broker = Broker::new();
        let publisher = broker.connect();
        let mut subscriber = publisher.subscribe("eieio/sys/t");

        for n in 1..=(SUBSCRIBER_CAPACITY as i64 + 5) {
            assert_eq!(publisher.publish("eieio/sys/t", batch(n)), Delivery::Sent);
        }
        assert!(
            broker.dropped() > 0,
            "a subscriber that never drained lost at least one batch"
        );

        // What the subscriber does receive is still in publish order, and it is not blocked
        // waiting for a drain that never happens — the publish loop above already proved that
        // by finishing at all.
        assert_eq!(
            n_of(&subscriber.recv().await.expect("the oldest surviving batch")),
            1
        );
    }

    #[tokio::test]
    async fn disconnecting_stops_delivery_without_a_network_to_sever() {
        let broker = Broker::new();
        let publisher = broker.connect();
        let mut subscriber = publisher.subscribe("eieio/sys/t");

        assert_eq!(publisher.publish("eieio/sys/t", batch(1)), Delivery::Sent);
        assert_eq!(n_of(&subscriber.recv().await.expect("connected")), 1);

        publisher.disconnect();
        assert!(!publisher.is_connected());
        assert_eq!(
            publisher.publish("eieio/sys/t", batch(2)),
            Delivery::Dropped
        );
        assert!(subscriber.rx.try_recv().is_err(), "nothing more arrived");
    }

    /// The acceptance scenario in full (DAEMON §6.3, §7): two daemons, one broker, one bus,
    /// a publisher on one and a subscriber on the other, and a batch that crosses between
    /// them with no network anywhere in this test — the in-process transport standing in for
    /// the broker exactly the way the module docs describe.
    ///
    /// Each "node" is its own `Executor`, which is what actually makes this two daemons and
    /// not one instance talking to itself: every piece of daemon machinery an instance runs
    /// on — its own thread, its own mailbox, its own `Events` stream — is duplicated, and the
    /// only thing shared between them is the `Broker`, standing in for the one thing two real
    /// daemons would share, a network path to one MQTT broker.
    #[tokio::test]
    async fn a_publisher_on_one_node_reaches_a_subscriber_on_another() {
        use crate::engine::Budgets;
        use crate::executor::{Event, Executor, Work};
        use crate::instance::{InstanceSpec, Origin};
        use eio_host_core::Limits;

        let broker = Broker::new();
        let bus = String::from("greenhouse");
        let limits = Limits::new(64 * 1024, 1024, None);
        let mut props = BTreeMap::new();
        props.insert(String::from("topic"), String::from("\"temperature\""));

        let node_a = Executor::new(Budgets::default(), 8)
            .expect("node A's executor")
            .bridging(Arc::new(broker.connect()), bus.clone());
        let node_b = Executor::new(Budgets::default(), 8)
            .expect("node B's executor")
            .bridging(Arc::new(broker.connect()), bus.clone());

        let (publisher, mut publisher_events) = node_a
            .spawn(InstanceSpec {
                origin: Origin::HostNative(SystemBlockKind::Publisher),
                registry: None,
                props: props.clone(),
                instance: None,
                service: String::from("s"),
                limits,
            })
            .await
            .expect("the publisher starts");
        let (subscriber, mut subscriber_events) = node_b
            .spawn(InstanceSpec {
                origin: Origin::HostNative(SystemBlockKind::Subscriber),
                registry: None,
                props,
                instance: None,
                service: String::from("s"),
                limits,
            })
            .await
            .expect("the subscriber starts");

        publisher
            .mailbox()
            .send(Work::Deliver {
                input_port: 0,
                batch: batch(7),
            })
            .await
            .expect("the publisher is running");

        // What "arrives" means for an instance with no service around it (DAEMON §6.3's
        // subscriber still reports what it emitted on `out`, the same as any block's `emit`
        // does): the batch decoded off the bridge, on the far side of a broker this test
        // never touches directly.
        let arrived = loop {
            match subscriber_events
                .recv()
                .await
                .expect("the subscriber is still running")
            {
                Event::Emitted { emission, .. } => break emission.batch,
                _ => continue,
            }
        };
        assert_eq!(
            n_of(&arrived),
            7,
            "the batch node B received is the one node A published"
        );

        publisher
            .mailbox()
            .send(Work::Stop)
            .await
            .expect("still running");
        subscriber
            .mailbox()
            .send(Work::Stop)
            .await
            .expect("still running");
        publisher.join();
        subscriber.join();
        while publisher_events.recv().await.is_some() {}
        while subscriber_events.recv().await.is_some() {}
    }

    /// The loss path, at the level a deployer actually observes it: a publisher's own
    /// `Events` stream, not just the bridge's internal counter (which
    /// [`a_disconnected_bridge_drops_and_counts_rather_than_sending`] already proves).
    /// SCOPE §3.4's "a publisher that cannot send drops" is asserted end to end — through
    /// `InstanceSpec`, the executor, the instance's own thread, and back out — rather than
    /// assumed from the bridge unit test alone.
    #[tokio::test]
    async fn a_publisher_reports_a_bridge_drop_on_its_own_event_stream() {
        use crate::engine::Budgets;
        use crate::executor::{Event, Executor, Work};
        use crate::instance::{InstanceSpec, Origin};
        use eio_host_core::Limits;

        let broker = Broker::new();
        // Kept alongside the `Arc<dyn Bridge>` the executor gets: `InProcessBridge` is a
        // handle around shared state (`Clone`, not a new connection), so disconnecting this
        // one disconnects the instance's too — a test's way of severing what has no socket to
        // sever.
        let bridge = broker.connect();
        let executor = Executor::new(Budgets::default(), 8)
            .expect("an executor")
            .bridging(Arc::new(bridge.clone()), String::from("greenhouse"));

        let mut props = BTreeMap::new();
        props.insert(String::from("topic"), String::from("\"temperature\""));
        let (publisher, mut events) = executor
            .spawn(InstanceSpec {
                origin: Origin::HostNative(SystemBlockKind::Publisher),
                registry: None,
                props,
                instance: None,
                service: String::from("s"),
                limits: Limits::new(64 * 1024, 1024, None),
            })
            .await
            .expect("the publisher starts");

        bridge.disconnect();
        publisher
            .mailbox()
            .send(Work::Deliver {
                input_port: 0,
                batch: batch(1),
            })
            .await
            .expect("still running");
        publisher
            .mailbox()
            .send(Work::Stop)
            .await
            .expect("still running");
        publisher.join();

        let mut saw_drop = false;
        while let Some(event) = events.recv().await {
            if let Event::BridgeDropped { topic } = event {
                assert_eq!(topic, "eieio/greenhouse/temperature");
                saw_drop = true;
            }
        }
        assert!(
            saw_drop,
            "a dropped publish must be visible on the instance's own event stream, not only \
             inside the bridge"
        );
        assert_eq!(bridge.dropped(), 1);
    }

    // ── `MqttBridge` (DAEMON §7, §7.1) ───────────────────────────────────────────────────────
    //
    // No test below reaches a non-loopback address, and none requires a broker anyone started
    // by hand: `spawn_embedded_broker` below runs `rumqttd` — a dev-dependency, not part of
    // this crate's own tree — in this process, on an OS-assigned `127.0.0.1` port. See the
    // module docs' "This module ships two transports" section for why that is the whole of
    // this issue's test story.

    #[test]
    fn candidate_parses_the_spec_example() {
        // DAEMON §7.1's own example, verbatim.
        let candidate = Candidate::parse("n7k2p4qv@10.0.0.5:1883").unwrap();
        assert_eq!(candidate.id, "n7k2p4qv");
        assert_eq!(candidate.host, "10.0.0.5");
        assert_eq!(candidate.port, 1883);
    }

    #[test]
    fn candidate_refuses_a_missing_at() {
        assert!(Candidate::parse("10.0.0.5:1883").is_err());
    }

    #[test]
    fn candidate_refuses_a_missing_port() {
        assert!(Candidate::parse("n7k2p4qv@10.0.0.5").is_err());
    }

    #[test]
    fn candidate_refuses_a_port_that_is_not_a_number() {
        assert!(Candidate::parse("n7k2p4qv@10.0.0.5:mqtt").is_err());
    }

    #[test]
    fn candidate_refuses_an_empty_id_or_host() {
        assert!(Candidate::parse("@10.0.0.5:1883").is_err());
        assert!(Candidate::parse("n7k2p4qv@:1883").is_err());
    }

    /// A free `127.0.0.1` port nothing is listening on yet — used both to size an embedded
    /// broker's listener and, dropped instead of handed to one, to name an address a candidate
    /// walk will never get an answer from.
    fn free_loopback_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("binding an ephemeral loopback port")
            .local_addr()
            .expect("reading back the bound address")
            .port()
    }

    /// Runs a real `rumqttd` broker, plain TCP, on its own OS thread for the life of the test
    /// process — the mechanism the module docs describe: a broker as a library dependency
    /// rather than a process a person or CI has to start. Returns the loopback port it is
    /// listening on.
    ///
    /// `auth`, when given, is `rumqttd`'s own static `(username, password)` credential check —
    /// the fixture's way of requiring the key this issue adds, since `rumqttd` has no key
    /// concept of its own, only an MQTT username/password pair. `None` is every pre-existing
    /// caller: an open broker, exactly as before this parameter existed.
    fn spawn_embedded_broker(auth: Option<(&str, &str)>) -> u16 {
        let port = free_loopback_port();
        let auth = auth.map(|(username, password)| {
            let mut pairs = HashMap::new();
            pairs.insert(String::from(username), String::from(password));
            pairs
        });
        let mut v4 = HashMap::new();
        v4.insert(
            String::from("1"),
            rumqttd::ServerSettings {
                name: String::from("test"),
                listen: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                tls: None,
                next_connection_delay_ms: 1,
                connections: rumqttd::ConnectionSettings {
                    connection_timeout_ms: 5_000,
                    max_payload_size: 20_480,
                    max_inflight_count: 100,
                    auth,
                    external_auth: None,
                    dynamic_filters: false,
                },
            },
        );
        let config = rumqttd::Config {
            id: 0,
            router: rumqttd::RouterConfig {
                max_connections: 10,
                max_outgoing_packet_count: 200,
                max_segment_size: 1024 * 1024,
                max_segment_count: 10,
                custom_segment: None,
                initialized_filters: None,
                shared_subscriptions_strategy: Default::default(),
            },
            v4: Some(v4),
            v5: None,
            ws: None,
            cluster: None,
            console: None,
            bridge: None,
            prometheus: None,
            metrics: None,
        };
        std::thread::Builder::new()
            .name(String::from("test-embedded-broker"))
            .spawn(move || {
                let mut broker = rumqttd::Broker::new(config);
                // `start` blocks for as long as the broker runs, which for this test is the
                // life of the process — there is no shutdown call to make, the same way the
                // test never joins the thread.
                let _ = broker.start();
            })
            .expect("spawning the embedded broker's thread");
        port
    }

    /// Polls `connected` until it is `true` or `timeout` elapses, panicking in the latter case
    /// with `what` — a small wait loop rather than a fixed sleep, since a fixed sleep is either
    /// too short under load or, made safely long, the slowest thing about every test using it.
    async fn wait_connected(bridge: &MqttBridge, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !bridge.is_connected() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what} never connected"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn mqtt_bridge_delivers_a_publish_across_a_real_broker() {
        let port = spawn_embedded_broker(None);
        let candidates = vec![Candidate {
            id: String::from("only"),
            host: String::from("127.0.0.1"),
            port,
        }];

        let publisher =
            MqttBridge::connect(String::from("publisher"), candidates.clone(), None, None);
        let subscriber = MqttBridge::connect(String::from("subscriber"), candidates, None, None);
        wait_connected(&publisher, "the publisher").await;
        wait_connected(&subscriber, "the subscriber").await;

        let mut sub = subscriber.subscribe("eieio/greenhouse/temperature");
        // The subscribe request above is asynchronous by construction (`Bridge::subscribe`
        // does not block on the round trip to the broker) — give it a moment to land before
        // the publish that must reach it.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            publisher.publish("eieio/greenhouse/temperature", batch(9)),
            Delivery::Sent
        );

        let arrived = tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("a batch arrived before the timeout")
            .expect("the subscription is still open");
        assert_eq!(n_of(&arrived), 9, "node B received what node A published");
    }

    #[tokio::test]
    async fn mqtt_bridge_never_retains() {
        // The real-transport half of what `a_subscription_never_sees_what_was_published_
        // before_it_existed` already proves for the in-process transport: DAEMON §7's "retained
        // messages are never set" over an actual MQTT broker, not just this crate's stand-in.
        let port = spawn_embedded_broker(None);
        let candidates = vec![Candidate {
            id: String::from("only"),
            host: String::from("127.0.0.1"),
            port,
        }];

        let publisher =
            MqttBridge::connect(String::from("publisher"), candidates.clone(), None, None);
        wait_connected(&publisher, "the publisher").await;
        assert_eq!(
            publisher.publish("eieio/greenhouse/temperature", batch(1)),
            Delivery::Sent
        );
        // Give the publish time to actually reach the broker before anyone subscribes late.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let late = MqttBridge::connect(String::from("late"), candidates, None, None);
        wait_connected(&late, "the late subscriber").await;
        let mut sub = late.subscribe("eieio/greenhouse/temperature");

        let result = tokio::time::timeout(Duration::from_millis(500), sub.recv()).await;
        assert!(
            result.is_err(),
            "a subscriber that connected after the publish must not receive it: retained was \
             not set (DAEMON §7)"
        );
    }

    #[tokio::test]
    async fn mqtt_bridge_drops_and_counts_when_nothing_answers() {
        // No broker anywhere, real or embedded — a loopback port dropped the instant it is
        // free again is DAEMON §7.1's "nothing reachable", the ordinary case rather than an
        // error, proved with no network beyond the loopback interface and nothing running.
        let port = free_loopback_port();
        let candidates = vec![Candidate {
            id: String::from("nobody-home"),
            host: String::from("127.0.0.1"),
            port,
        }];
        let bridge = MqttBridge::connect(String::from("node"), candidates, None, None);

        // Long enough for at least one full walk-and-fail, short of the retry actually firing
        // twice — either way `is_connected` must read false, never block.
        tokio::time::sleep(DIAL_TIMEOUT + Duration::from_millis(200)).await;
        assert!(!bridge.is_connected());

        assert_eq!(
            bridge.publish("eieio/greenhouse/temperature", batch(1)),
            Delivery::Dropped,
            "a publish with no broker connection must drop rather than block or error out"
        );
        assert_eq!(bridge.dropped(), 1, "the drop was logged and counted");
        assert_eq!(
            bridge.rejected(),
            0,
            "nothing answered at all, so nothing could have refused the connection either"
        );
    }

    // ── the bus key (SCOPE §3.11, DAEMON §7.1, eieio-2vm.5) ──────────────────────────────────

    #[tokio::test]
    async fn mqtt_bridge_connects_when_its_key_matches_what_the_broker_requires() {
        let port = spawn_embedded_broker(Some((MQTT_USERNAME, "the-bus-key")));
        let candidates = vec![Candidate {
            id: String::from("only"),
            host: String::from("127.0.0.1"),
            port,
        }];

        let bridge = MqttBridge::connect(
            String::from("node"),
            candidates,
            None,
            Some(crate::pubsub::Key::new("the-bus-key")),
        );
        wait_connected(&bridge, "the bridge").await;
        assert_eq!(
            bridge.rejected(),
            0,
            "the right key must never be counted as a rejection"
        );
    }

    #[tokio::test]
    async fn mqtt_bridge_still_connects_with_no_key_against_an_unkeyed_broker() {
        // The acceptance criterion in full: "`key` absent = no key presented, which is the
        // current behaviour and must keep working". `None` here is the same argument every
        // pre-existing test in this module already passes; this test just names the behaviour
        // its own criterion rather than leaving it implicit in the others.
        let port = spawn_embedded_broker(None);
        let candidates = vec![Candidate {
            id: String::from("only"),
            host: String::from("127.0.0.1"),
            port,
        }];

        let bridge = MqttBridge::connect(String::from("node"), candidates, None, None);
        wait_connected(&bridge, "the bridge").await;
    }

    /// The acceptance criterion this issue is most likely to fudge, made real: a wrong key must
    /// be distinguishable from an unreachable broker, not just refused the same way. Compares
    /// directly against [`mqtt_bridge_drops_and_counts_when_nothing_answers`]'s counts for the
    /// unreachable case — that test never observes a `rejected` count, this one never observes
    /// its connection actually succeed, and neither ever confuses `dropped` (a publish lost
    /// after connecting) with `rejected` (a connection that was never accepted at all).
    #[tokio::test]
    async fn mqtt_bridge_distinguishes_a_wrong_key_from_an_unreachable_broker() {
        let port = spawn_embedded_broker(Some((MQTT_USERNAME, "the-real-key")));
        let candidates = vec![Candidate {
            id: String::from("only"),
            host: String::from("127.0.0.1"),
            port,
        }];

        let bridge = MqttBridge::connect(
            String::from("node"),
            candidates,
            None,
            Some(crate::pubsub::Key::new("a-wrong-key")),
        );

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while bridge.rejected() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the wrong key was never observed as a rejection"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert!(
            !bridge.is_connected(),
            "a wrong key must never leave the bridge connected"
        );
        assert_eq!(
            bridge.dropped(),
            0,
            "nothing was ever connected, so nothing could have been dropped from a \
             connection either — this failure is `rejected`, not `dropped`"
        );
    }

    /// The other half of the same criterion: a *missing* key against a broker that requires one
    /// must be refused too, and refused the same distinguishable way as a wrong one — DAEMON
    /// §7.1's candidate "requires it" (this issue's own acceptance criterion), not merely
    /// "prefers it".
    #[tokio::test]
    async fn mqtt_bridge_distinguishes_a_missing_key_from_an_unreachable_broker() {
        let port = spawn_embedded_broker(Some((MQTT_USERNAME, "the-real-key")));
        let candidates = vec![Candidate {
            id: String::from("only"),
            host: String::from("127.0.0.1"),
            port,
        }];

        let bridge = MqttBridge::connect(String::from("node"), candidates, None, None);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while bridge.rejected() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "presenting no key was never observed as a rejection"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!bridge.is_connected());
    }

    #[tokio::test]
    async fn a_pin_dials_only_the_pinned_candidate() {
        // Two candidates: one a real embedded broker that would answer if tried, one an
        // address nothing is listening on. Ranked first, the reachable one would normally win
        // DAEMON §7.1's walk — pinning the unreachable one must still leave this bridge
        // disconnected, because "while it is present, candidates do not self-promote".
        let reachable_port = spawn_embedded_broker(None);
        let unreachable_port = free_loopback_port();
        let candidates = vec![
            Candidate {
                id: String::from("reachable"),
                host: String::from("127.0.0.1"),
                port: reachable_port,
            },
            Candidate {
                id: String::from("pinned-target"),
                host: String::from("127.0.0.1"),
                port: unreachable_port,
            },
        ];

        let bridge = MqttBridge::connect(
            String::from("node"),
            candidates,
            Some(String::from("pinned-target")),
            None,
        );
        tokio::time::sleep(DIAL_TIMEOUT + Duration::from_millis(200)).await;
        assert!(
            !bridge.is_connected(),
            "a pin naming an unreachable candidate must not fall back to a higher-ranked one"
        );
    }

    /// The mechanical enforcement of DAEMON §7's boundary: no other module in this crate may
    /// name a transport concept. This is a source scan and not a trait-bound check because the
    /// property being proved is exactly "nobody wrote the word", which the type system has no
    /// way to ask about — a signature that *could* name `rumqttc::QoS` is refused by the
    /// compiler the moment someone writes it, but only if nobody imports it under another name,
    /// which is what this test actually checks. See the deliberate-leak note below for how this
    /// was verified to fail.
    #[test]
    fn the_bridge_boundary_holds() {
        // Transport vocabulary that belongs to a concrete `impl Bridge` and never above it
        // (DAEMON §7): MQTT's own concepts, and this module's private broker type — a caller
        // outside this file has no business naming either, only `Bridge`, `Delivery` and
        // `Subscription`.
        const FORBIDDEN: &[&str] = &[
            "rumqttc",
            "MqttOptions",
            "QoS",
            "retained message",
            "Broker",
        ];

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations = Vec::new();
        walk(&src, &mut |path| {
            // This file is the bridge implementation; everything else is "above" it.
            if path == std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bridge.rs") {
                return;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                return;
            };
            for &word in FORBIDDEN {
                if text.contains(word) {
                    violations.push(format!("{}: names `{word}`", path.display()));
                }
            }
        });

        assert!(
            violations.is_empty(),
            "a transport concept leaked outside the bridge module:\n{}",
            violations.join("\n")
        );
    }

    /// Every `.rs` file under `dir`, recursively.
    fn walk(dir: &std::path::Path, visit: &mut impl FnMut(&std::path::Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, visit);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                visit(&path);
            }
        }
    }
}
