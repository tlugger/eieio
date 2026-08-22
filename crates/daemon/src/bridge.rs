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
//! # This module ships one transport: in-process
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
//! `rumqttc` itself is not a dependency of this crate yet (eieio-2vm.2's scope decision): every
//! `rumqttc` API that would carry a real MQTT connection either needs a live broker process or
//! blocks on the network in a way this crate's tests must not. Wiring it in is a follow-up with
//! its own test story, not a comment here.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
        let limits = Limits::new(64 * 1024, 1024);
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
                limits: Limits::new(64 * 1024, 1024),
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
