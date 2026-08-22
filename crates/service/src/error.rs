//! What a service file can be wrong about (SERVICE-SPEC §7).
//!
//! Every class is a distinct variant, because "distinct" in §7 means a caller can tell them
//! apart without matching on a message. The Designer renders a failure on the offending
//! block, property or connection (DESIGNER §5), and it cannot do that from a string.

use std::fmt;

/// Where something sat in the text it was found in.
///
/// Byte offsets, like EXPR §8's spans, and for the same reason: an editor needs to underline
/// the thing, not be told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first character.
    pub start: usize,
    /// Length in bytes.
    pub len: usize,
}

impl Span {
    /// A span of `len` bytes at `start`.
    pub const fn new(start: usize, len: usize) -> Span {
        Span { start, len }
    }
}

/// What a connection string can be wrong about (SERVICE §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionError {
    /// No `->` at all.
    NoArrow,
    /// More than one `->`: an edge has two ends.
    TwoArrows,
    /// Nothing on one side of the arrow.
    EmptyTerminal {
        /// `"source"` or `"destination"`.
        side: &'static str,
    },
    /// A terminal with no `.`, so no port was named.
    NoPort {
        /// Which side.
        side: &'static str,
        /// Where it sat.
        span: Span,
    },
    /// A terminal with two dots. Ids and ports exclude `.` precisely so this is unambiguous.
    TwoDots {
        /// Which side.
        side: &'static str,
        /// Where it sat.
        span: Span,
    },
    /// The instance half does not satisfy SERVICE §2.1.
    BadInstance {
        /// Which side.
        side: &'static str,
        /// Where it sat.
        span: Span,
    },
    /// The port half does not satisfy the same pattern.
    BadPort {
        /// Which side.
        side: &'static str,
        /// Where it sat.
        span: Span,
    },
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionError::NoArrow => {
                write!(
                    f,
                    "a connection is `<id>.<port> -> <id>.<port>`, and there is no `->`"
                )
            }
            ConnectionError::TwoArrows => write!(f, "more than one `->`: an edge has two ends"),
            ConnectionError::EmptyTerminal { side } => write!(f, "the {side} is empty"),
            ConnectionError::NoPort { side, .. } => {
                write!(f, "the {side} names no port: it is `<id>.<port>`")
            }
            ConnectionError::TwoDots { side, .. } => {
                write!(
                    f,
                    "the {side} has two dots, and neither ids nor ports may contain one"
                )
            }
            ConnectionError::BadInstance { side, .. } => write!(
                f,
                "the {side}'s instance id does not match {} (SERVICE §2.1)",
                crate::id::ID_PATTERN
            ),
            ConnectionError::BadPort { side, .. } => write!(
                f,
                "the {side}'s port name does not match {} (ABI §11.1)",
                crate::id::ID_PATTERN
            ),
        }
    }
}

/// Everything SERVICE §7 stage 1 can find, from the file alone.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The document is not TOML, or does not fit the schema (§3, §4).
    ///
    /// Carries the message the parser produced, which already names the line and the key —
    /// restating it here would be a second, worse copy.
    ///
    /// **Two of §7's classes arrive here together**, and that is a limit rather than a
    /// choice: malformed TOML and an unknown field are both `toml::de::Error`, which does not
    /// say structurally which it was. Splitting them would mean matching on its message,
    /// which is the thing this enum exists to spare a caller. SERVICE §7 says so too.
    Toml(String),

    /// The service's own name does not satisfy SERVICE §3.
    ServiceName {
        /// What was written.
        name: String,
    },

    /// The top-level `overflow` key names a policy SERVICE §5 does not define.
    Overflow {
        /// What was written.
        value: String,
    },

    /// A block instance's id does not satisfy SERVICE §2.1.
    InstanceId {
        /// The offending key.
        id: String,
    },

    /// An instance's `block` reference is empty (SERVICE §4).
    EmptyBlockRef {
        /// The instance.
        id: String,
    },

    /// A connection string that does not parse (§5).
    ConnectionSyntax {
        /// Which entry of `connections`.
        index: usize,
        /// The string as written, so a caller can render the span against it.
        text: String,
        /// What was wrong.
        error: ConnectionError,
    },

    /// A connection naming an instance the file does not define (§7).
    DanglingConnection {
        /// Which entry.
        index: usize,
        /// The id that is not there.
        instance: String,
        /// Which side named it.
        side: &'static str,
    },

    /// The same edge twice (§5).
    DuplicateConnection {
        /// The later entry.
        index: usize,
        /// The earlier one it repeats.
        first: usize,
    },

    /// `err` used as a destination. It is an output port (ABI §6.4).
    ErrorPortDestination {
        /// Which entry.
        index: usize,
    },

    /// A property expression `expr` rejected — either at parse or in static analysis
    /// (EXPR §8, §10).
    ///
    /// Carries EXPR §8's code rather than a rendering of it, which is what makes SERVICE §7's
    /// last two rows two classes instead of one: `ErrorCode::Parse` is an expression that is
    /// not an expression, and everything else is one that cannot mean anything. A caller that
    /// had to grep a message for "UNBOUND" would be matching on prose.
    Property {
        /// The instance.
        id: String,
        /// The property.
        property: String,
        /// Which of EXPR §8's codes.
        code: eio_expr::ErrorCode,
        /// Where in the expression, so an editor can underline it.
        span: eio_expr::Span,
        /// What `expr` said, for a human.
        message: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Toml(detail) => write!(f, "{detail}"),
            Error::ServiceName { name } => write!(
                f,
                "the service name {name:?} does not match {} (SERVICE §3)",
                crate::id::ID_PATTERN
            ),
            Error::InstanceId { id } => write!(
                f,
                "the block instance id {id:?} does not match {} (SERVICE §2.1)",
                crate::id::ID_PATTERN
            ),
            Error::Overflow { value } => write!(
                f,
                "overflow = {value:?} is not a recognised policy; expected one of {:?} (SERVICE §5)",
                crate::overflow::Overflow::ACCEPTED
            ),
            Error::EmptyBlockRef { id } => {
                write!(f, "block instance {id:?} names no block (SERVICE §4)")
            }
            Error::ConnectionSyntax { index, text, error } => {
                write!(f, "connection {index} ({text:?}): {error}")
            }
            Error::DanglingConnection {
                index,
                instance,
                side,
            } => write!(
                f,
                "connection {index}: the {side} names {instance:?}, which this service does not define"
            ),
            Error::DuplicateConnection { index, first } => {
                write!(f, "connection {index} repeats connection {first}")
            }
            Error::ErrorPortDestination { index } => write!(
                f,
                "connection {index}: `err` is an output port and cannot be a destination (ABI §6.4)"
            ),
            Error::Property {
                id,
                property,
                code,
                span,
                message,
            } => write!(f, "{id}.{property}: {code:?} at {span}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// What SERVICE §7 stage 2 can find, once the blocks are resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedError {
    /// A connection naming a port the block does not declare.
    UnknownPort {
        /// Which entry of `connections`.
        index: usize,
        /// The instance.
        instance: String,
        /// The port named.
        port: String,
        /// `"output"` or `"input"` — which direction was wanted.
        direction: &'static str,
    },

    /// A configured property the block does not declare (ABI §11).
    UnknownProperty {
        /// The instance.
        id: String,
        /// The property configured.
        property: String,
    },
}

impl fmt::Display for ResolvedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolvedError::UnknownPort {
                index,
                instance,
                port,
                direction,
            } => write!(
                f,
                "connection {index}: {instance:?} declares no {direction} port {port:?}"
            ),
            ResolvedError::UnknownProperty { id, property } => {
                write!(f, "{id}: the block declares no property {property:?}")
            }
        }
    }
}

impl std::error::Error for ResolvedError {}
