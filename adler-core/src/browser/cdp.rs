//! Minimal async Chrome `DevTools` Protocol client.
//!
//! Maintained Rust CDP libraries (`chromiumoxide`, `headless_chrome`) both
//! assume target-event semantics that match a *locally launched* Chrome and
//! deadlock against cloud CDP brokers (Browserbase) that don't fire those
//! events the same way. This module is a deliberately small alternative:
//! it doesn't model CDP at all, it just exposes a typed request/response
//! channel plus an event stream. Higher-level backends compose the few
//! commands they need on top.
//!
//! Wire layout (CDP "flatten" mode, the modern default):
//!
//! - Request:  `{"id": N, "method": "Domain.cmd", "params": {...}, "sessionId": "..."}`
//! - Response: `{"id": N, "result": {...}}` or `{"id": N, "error": {"code": -32601, "message": "..."}}`
//! - Event:    `{"method": "Domain.event", "params": {...}, "sessionId": "..."}`
//!
//! [`CdpClient`] owns the WebSocket, spawns a background read task that
//! demultiplexes responses (matched by `id`) from events (broadcast to
//! subscribers), and exposes a typed [`execute`](CdpClient::execute) call
//! plus [`subscribe_events`](CdpClient::subscribe_events).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_tungstenite::tokio::{ConnectStream, connect_async};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::error::Error as WsError;
use async_tungstenite::{WebSocketReceiver, WebSocketSender};
use futures::StreamExt as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};
use tokio::task::JoinHandle;

type Sink = WebSocketSender<ConnectStream>;
type Stream = WebSocketReceiver<ConnectStream>;

/// Default channel capacity for the event broadcaster. Bigger than any
/// single CDP burst we've observed; subscribers that fall behind lose
/// older events (see [`broadcast::Receiver::recv`] lag semantics).
const EVENT_BUFFER: usize = 256;

/// Errors a [`CdpClient`] can surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CdpError {
    /// WebSocket transport failure (handshake, TLS, abrupt close, …).
    #[error("websocket: {0}")]
    WebSocket(String),

    /// CDP responded with `{"error": {"code": …, "message": …}}` for our
    /// command. Carries the protocol-level details.
    #[error("CDP {code}: {message}")]
    Remote {
        /// Numeric CDP error code (e.g. `-32601` for method-not-found).
        code: i64,
        /// Human-readable message from the remote.
        message: String,
    },

    /// Response decoded as JSON but didn't match the typed `R` we asked
    /// for. Wraps the underlying serde error.
    #[error("decode response: {0}")]
    Decode(String),

    /// `execute` (or `wait_for_event`) blocked past its deadline.
    #[error("CDP {what} timed out after {elapsed:?}")]
    Timeout {
        /// `Duration` the call waited before giving up.
        elapsed: Duration,
        /// Friendly label for the kind of wait (e.g. `"Page.navigate"`).
        what: &'static str,
    },

    /// The client was used after [`CdpClient::close`] (or the read loop
    /// exited because the peer closed the connection).
    #[error("CDP client is closed")]
    Closed,
}

impl CdpError {
    fn ws(e: &WsError) -> Self {
        Self::WebSocket(e.to_string())
    }
}

/// A CDP event delivered to subscribers of [`CdpClient::subscribe_events`].
#[derive(Debug, Clone)]
pub struct CdpEvent {
    /// `Domain.eventName`, e.g. `Network.responseReceived`.
    pub method: String,
    /// Event-specific payload. Decode with `serde_json::from_value`.
    pub params: Value,
    /// `Some(...)` for events scoped to an attached target (the typical
    /// case in flatten mode); `None` for browser-wide events.
    pub session_id: Option<String>,
}

#[derive(Serialize)]
struct Request<'a, P> {
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<P>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: i64,
    message: String,
}

/// One incoming frame from the WebSocket — either the response to a
/// previous request (when `id.is_some()`) or an unsolicited event.
#[derive(Deserialize)]
struct Frame {
    id: Option<u64>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<RemoteError>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

type PendingMap = std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>;

struct Inner {
    sink: AsyncMutex<Sink>,
    pending: PendingMap,
    next_id: AtomicU64,
    events: broadcast::Sender<CdpEvent>,
    closed: AtomicBool,
}

impl Inner {
    fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Fail every in-flight request once.
        let drained: Vec<_> = {
            let mut g = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            g.drain().collect()
        };
        for (_, tx) in drained {
            let _ = tx.send(Err(CdpError::Closed));
        }
    }
}

/// Connected CDP client. Cheap to share via the trait object on a
/// backend — internal state is `Arc`-backed.
///
/// Dropping the client aborts its read task; pending in-flight calls
/// surface [`CdpError::Closed`].
pub struct CdpClient {
    inner: Arc<Inner>,
    read_loop: JoinHandle<()>,
}

impl fmt::Debug for CdpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CdpClient")
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .field(
                "pending",
                &self
                    .inner
                    .pending
                    .lock()
                    .map(|g| g.len())
                    .unwrap_or_default(),
            )
            .field("read_loop_finished", &self.read_loop.is_finished())
            .finish()
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        self.inner.mark_closed();
        self.read_loop.abort();
    }
}

impl CdpClient {
    /// Open a WebSocket to `url` (ws:// or wss://) and start the read
    /// loop. Returns once the handshake completes.
    ///
    /// # Errors
    /// [`CdpError::WebSocket`] on handshake, DNS, or TLS failure.
    pub async fn connect(url: &str) -> Result<Self, CdpError> {
        let (ws, _resp) = connect_async(url).await.map_err(|e| CdpError::ws(&e))?;
        let (sink, stream) = ws.split();
        let (events_tx, _) = broadcast::channel(EVENT_BUFFER);
        let inner = Arc::new(Inner {
            sink: AsyncMutex::new(sink),
            pending: std::sync::Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events: events_tx,
            closed: AtomicBool::new(false),
        });
        let read_loop = tokio::spawn(read_loop(Arc::clone(&inner), stream));
        Ok(Self { inner, read_loop })
    }

    /// Send `Domain.cmd` with `params`, await the matching response, and
    /// decode the `result` field as `R`.
    ///
    /// `session_id` scopes the call to a flat-attached target (see
    /// `Target.attachToTarget` with `flatten: true`); pass `None` for
    /// browser-wide commands.
    ///
    /// # Errors
    /// - [`CdpError::Closed`] if the client has been closed.
    /// - [`CdpError::WebSocket`] if the send fails on the wire.
    /// - [`CdpError::Timeout`] if no response arrives within `timeout`.
    /// - [`CdpError::Remote`] if CDP replied with an error object.
    /// - [`CdpError::Decode`] if the result didn't deserialise as `R`.
    pub async fn execute<P, R>(
        &self,
        method: &'static str,
        params: P,
        session_id: Option<&str>,
        timeout: Duration,
    ) -> Result<R, CdpError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(CdpError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::AcqRel);
        let req = Request {
            id,
            method,
            params: Some(params),
            session_id,
        };
        let json = serde_json::to_string(&req).map_err(|e| CdpError::Decode(e.to_string()))?;

        let (tx, rx) = oneshot::channel();
        // Reserve the slot *before* sending so a fast response can't race
        // ahead of the registration.
        {
            let mut g = self
                .inner
                .pending
                .lock()
                .map_err(|_| CdpError::WebSocket("pending mutex poisoned".into()))?;
            g.insert(id, tx);
        }

        // Hold the sink only for the duration of the write.
        let send = {
            let mut sink = self.inner.sink.lock().await;
            sink.send(Message::Text(json.into())).await
        };
        if let Err(e) = send {
            // Sender side broke — yank the slot so we don't leak it.
            let _ = self
                .inner
                .pending
                .lock()
                .map(|mut g| g.remove(&id))
                .unwrap_or_default();
            return Err(CdpError::ws(&e));
        }

        let wait = async {
            rx.await.map_err(|_| CdpError::Closed)?.and_then(|value| {
                serde_json::from_value::<R>(value).map_err(|e| CdpError::Decode(e.to_string()))
            })
        };

        tokio::time::timeout(timeout, wait).await.map_or_else(
            |_| {
                // Drop the (now-useless) channel entry so the read loop can
                // discard the eventual late response.
                let _ = self
                    .inner
                    .pending
                    .lock()
                    .map(|mut g| g.remove(&id))
                    .unwrap_or_default();
                Err(CdpError::Timeout {
                    elapsed: timeout,
                    what: method,
                })
            },
            |result| result,
        )
    }

    /// Subscribe to every event the read loop dispatches.
    ///
    /// Slow subscribers may lag — [`broadcast::Receiver::recv`] returns
    /// [`broadcast::error::RecvError::Lagged`] in that case. Filter the
    /// stream on `.method` and `.session_id` to scope to the events you
    /// care about.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    /// Convenience: open a fresh subscription and drive it until
    /// `predicate` returns `true`, or `timeout` elapses.
    ///
    /// Has a built-in race: if the event you're waiting for fires
    /// *between* the action that triggers it and this call, it's missed
    /// (the broadcast channel doesn't replay history). For event waits
    /// that follow a triggering command, prefer
    /// [`wait_for_event_on`](Self::wait_for_event_on) with a
    /// subscription opened **before** the trigger.
    ///
    /// # Errors
    /// [`CdpError::Timeout`] if `timeout` elapses; [`CdpError::Closed`]
    /// if the underlying stream ends first.
    pub async fn wait_for_event<F>(
        &self,
        predicate: F,
        timeout: Duration,
        what: &'static str,
    ) -> Result<CdpEvent, CdpError>
    where
        F: Fn(&CdpEvent) -> bool + Send + Sync,
    {
        let mut rx = self.subscribe_events();
        Self::wait_for_event_on(&mut rx, predicate, timeout, what).await
    }

    /// Drive an already-opened subscription until `predicate` returns
    /// `true`. Use this when you need to subscribe *before* sending the
    /// command that triggers the event — otherwise the event can fire
    /// before your subscription exists and you'll deadlock.
    ///
    /// # Errors
    /// Same as [`wait_for_event`](Self::wait_for_event).
    pub async fn wait_for_event_on<F>(
        rx: &mut broadcast::Receiver<CdpEvent>,
        predicate: F,
        timeout: Duration,
        what: &'static str,
    ) -> Result<CdpEvent, CdpError>
    where
        F: Fn(&CdpEvent) -> bool + Send + Sync,
    {
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(evt) if predicate(&evt) => return Ok::<CdpEvent, CdpError>(evt),
                    // not ours yet (or we lagged behind the broadcast); keep listening
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(CdpError::Closed),
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| CdpError::Timeout {
                elapsed: timeout,
                what,
            })?
    }

    /// Best-effort: close the WebSocket politely. Pending calls
    /// surface [`CdpError::Closed`]. Always safe to call; subsequent
    /// calls are no-ops.
    pub async fn close(self) {
        self.inner.mark_closed();
        let _ = self.inner.sink.lock().await.close(None).await;
        self.read_loop.abort();
    }
}

/// Read loop: pull frames off the WebSocket, dispatch responses by `id`
/// or broadcast as events. Exits when the stream ends or the client is
/// marked closed.
async fn read_loop(inner: Arc<Inner>, mut stream: Stream) {
    while let Some(msg) = stream.next().await {
        if inner.closed.load(Ordering::Acquire) {
            break;
        }
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(b)) => {
                let Ok(decoded) = String::from_utf8(b.into()) else {
                    tracing::warn!("CDP: non-UTF8 binary frame, dropped");
                    continue;
                };
                decoded.into()
            }
            Ok(Message::Close(_)) => {
                tracing::debug!("CDP: peer closed");
                break;
            }
            Ok(_) => continue, // Ping/Pong/Frame handled internally by tungstenite
            Err(e) => {
                tracing::warn!(error = %e, "CDP: stream error, closing");
                break;
            }
        };

        let frame: Frame = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "CDP: malformed frame, dropped");
                continue;
            }
        };

        match (frame.id, frame.method) {
            (Some(id), _) => {
                // Response to a command. Match by id.
                let tx = inner.pending.lock().ok().and_then(|mut g| g.remove(&id));
                if let Some(tx) = tx {
                    let result = if let Some(err) = frame.error {
                        Err(CdpError::Remote {
                            code: err.code,
                            message: err.message,
                        })
                    } else {
                        Ok(frame.result.unwrap_or(Value::Null))
                    };
                    let _ = tx.send(result);
                } else {
                    tracing::debug!(id, "CDP: response for unknown / cancelled id");
                }
            }
            (None, Some(method)) => {
                let evt = CdpEvent {
                    method,
                    params: frame.params.unwrap_or(Value::Null),
                    session_id: frame.session_id,
                };
                // Ignore SendError — having no subscribers is fine.
                let _ = inner.events.send(evt);
            }
            (None, None) => {
                tracing::warn!("CDP: frame has neither id nor method, dropped");
            }
        }
    }
    inner.mark_closed();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialises_with_optional_fields() {
        let r: Request<'_, Value> = Request {
            id: 42,
            method: "Page.enable",
            params: None,
            session_id: None,
        };
        let s = serde_json::to_value(&r).unwrap();
        // No null `params` / `sessionId` keys — keeps the wire compact.
        assert_eq!(s, serde_json::json!({ "id": 42, "method": "Page.enable" }));
    }

    #[test]
    fn request_serialises_with_session_id() {
        let r = Request {
            id: 7,
            method: "Page.navigate",
            params: Some(serde_json::json!({ "url": "https://example.com" })),
            session_id: Some("abc-123"),
        };
        let s = serde_json::to_value(&r).unwrap();
        assert_eq!(
            s,
            serde_json::json!({
                "id": 7,
                "method": "Page.navigate",
                "params": {"url": "https://example.com"},
                "sessionId": "abc-123",
            })
        );
    }

    #[test]
    fn frame_parses_a_response() {
        let txt = r#"{"id": 1, "result": {"targetId": "T1"}}"#;
        let f: Frame = serde_json::from_str(txt).unwrap();
        assert_eq!(f.id, Some(1));
        assert!(f.method.is_none());
        assert_eq!(f.result.unwrap(), serde_json::json!({"targetId": "T1"}));
    }

    #[test]
    fn frame_parses_a_remote_error() {
        let txt = r#"{"id": 9, "error": {"code": -32601, "message": "Method not found"}}"#;
        let f: Frame = serde_json::from_str(txt).unwrap();
        let err = f.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn frame_parses_an_event_with_session_id() {
        let txt =
            r#"{"method": "Page.loadEventFired", "params": {"timestamp": 1.0}, "sessionId": "S1"}"#;
        let f: Frame = serde_json::from_str(txt).unwrap();
        assert!(f.id.is_none());
        assert_eq!(f.method.as_deref(), Some("Page.loadEventFired"));
        assert_eq!(f.session_id.as_deref(), Some("S1"));
        assert!(f.params.is_some());
    }
}
