//! Turning a bus subscription into an SSE response (DAEMON-SPEC §9.6).
//!
//! Shared by taps and by `/logs/stream`, which are two filters over one bus and differ in
//! nothing else. It lives in its own module rather than in whichever of them was written first,
//! because neither is the other's host.

use std::convert::Infallible;
use std::sync::Arc;

use axum::response::sse::{Event as SseEvent, KeepAlive};
use futures_core::Stream;
use tokio::sync::broadcast::{Receiver, error::RecvError};

use crate::observe::{Observation, What, event};

/// Turns a bus subscription into SSE events, reporting lag rather than hiding it.
pub fn stream_of(
    mut receiver: Receiver<Arc<Observation>>,
    matches: impl Fn(&Observation) -> bool + Send + 'static,
) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    async_stream::stream! {
        loop {
            match receiver.recv().await {
                Ok(observation) if matches(&observation) => {
                    yield Ok(sse_event(observation.event, &observation));
                }
                Ok(_) => {}
                // DAEMON §9.6: a debugging tool that quietly showed a subset would be worse
                // than one that shows less and says so.
                Err(RecvError::Lagged(missed)) => {
                    yield Ok(sse_event(
                        event::LAGGED,
                        &Observation {
                            service: String::new(),
                            instance: String::new(),
                            event: event::LAGGED,
                            port: None,
                            what: What::Lagged { missed },
                        },
                    ));
                }
                // The bus is gone, which means the node is going down.
                Err(RecvError::Closed) => break,
            }
        }
    }
}

/// One SSE event, named and JSON-bodied.
fn sse_event(name: &str, observation: &Observation) -> SseEvent {
    SseEvent::default()
        .event(name)
        .json_data(observation)
        .unwrap_or_else(|error| {
            SseEvent::default()
                .event("error")
                .data(format!("this observation could not be rendered: {error}"))
        })
}

/// Keeps an idle stream open through proxies and NAT.
///
/// A tap on a quiet connection is the normal case — an operator opens one *before* the thing
/// they are waiting for happens — so a stream that was closed for being idle would be closed
/// exactly when it was most wanted.
pub fn keep_alive() -> KeepAlive {
    KeepAlive::new().interval(std::time::Duration::from_secs(15))
}
