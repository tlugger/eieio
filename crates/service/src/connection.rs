//! `"<id>.<port> -> <id>.<port>"` (SERVICE-SPEC §5).
//!
//! A grammar small enough to state in full, parsed by hand for one reason: every failure has
//! to say *where*. The Designer renders a validation error on the offending connection
//! (DESIGNER §5), and a parser that answered "invalid connection" would leave it with a red
//! line and nothing to underline.

use crate::error::{ConnectionError, Span};
use crate::id;

/// The arrow, and the whole of what separates a source from a destination.
const ARROW: &str = "->";

/// One end of an edge: an instance id and a port name on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// The instance's id (SERVICE §2).
    pub instance: String,
    /// The port's name. `err` on a source is ABI §6.4's reserved port.
    pub port: String,
    /// Where the instance id sat in the original string.
    pub instance_span: Span,
    /// Where the port name sat.
    pub port_span: Span,
}

/// One edge, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// Where signals come from: an *output* port.
    pub from: Terminal,
    /// Where they go: an *input* port.
    pub to: Terminal,
}

impl Connection {
    /// Parses one connection string.
    ///
    /// Whitespace is permitted around the arrow and nowhere else (SERVICE §5): `"a .out"` is
    /// a typo, and reading it as `a.out` would teach that the format guesses.
    pub fn parse(text: &str) -> Result<Connection, ConnectionError> {
        let Some(arrow) = text.find(ARROW) else {
            return Err(ConnectionError::NoArrow);
        };
        // The *first* arrow, and then a check that it was the only one: two arrows is a
        // three-terminal edge, which is not a thing, and silently taking the first would
        // wire something the author did not write.
        if text[arrow + ARROW.len()..].contains(ARROW) {
            return Err(ConnectionError::TwoArrows);
        }

        let from = Terminal::parse(text, 0, arrow, "source")?;
        let to = Terminal::parse(text, arrow + ARROW.len(), text.len(), "destination")?;
        Ok(Connection { from, to })
    }
}

impl Terminal {
    /// Parses `<id>.<port>` out of `text[start..end]`, keeping the spans absolute.
    fn parse(
        text: &str,
        start: usize,
        end: usize,
        side: &'static str,
    ) -> Result<Terminal, ConnectionError> {
        let raw = &text[start..end];
        // Only the outer whitespace, and only next to the arrow or the ends of the string.
        // What is left has to be exactly `id.port` with nothing between.
        let trimmed = raw.trim();
        let offset = start + (raw.len() - raw.trim_start().len());

        if trimmed.is_empty() {
            return Err(ConnectionError::EmptyTerminal { side });
        }
        let Some(dot) = trimmed.find('.') else {
            return Err(ConnectionError::NoPort {
                side,
                span: Span::new(offset, trimmed.len()),
            });
        };
        if trimmed[dot + 1..].contains('.') {
            return Err(ConnectionError::TwoDots {
                side,
                span: Span::new(offset, trimmed.len()),
            });
        }

        let instance = &trimmed[..dot];
        let port = &trimmed[dot + 1..];
        let instance_span = Span::new(offset, instance.len());
        let port_span = Span::new(offset + dot + 1, port.len());

        // The same rule on both halves, named for what each is: SERVICE §2.1's id rule *is*
        // ABI §11.1's port rule (`id::is_id` re-exports it), which is what keeps a terminal
        // parseable — both halves are TOML bare keys that cannot contain the dot between
        // them.
        if !id::is_id(instance) {
            return Err(ConnectionError::BadInstance {
                side,
                span: instance_span,
            });
        }
        if !eio_manifest::is_port_name(port) {
            return Err(ConnectionError::BadPort {
                side,
                span: port_span,
            });
        }

        Ok(Terminal {
            instance: instance.to_string(),
            port: port.to_string(),
            instance_span,
            port_span,
        })
    }
}
