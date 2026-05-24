//! In-process WebSocket server that speaks just enough Chrome `DevTools`
//! Protocol to drive [`super::cdp::CdpClient`] and
//! [`super::browserbase::BrowserbaseBackend`] from tests, without
//! launching a real browser or paying a cloud session.
//!
//! ## Scope
//!
//! Not a fully-faithful CDP implementation. The mock owns one
//! incoming connection at a time, decodes JSON-RPC frames, and
//! consults a caller-supplied handler closure for the response (and
//! any follow-up events). Anything the handler doesn't know about
//! gets a generic `{}` result so tests don't have to enumerate every
//! enable / disable / configure command.
//!
//! ## Usage
//!
//! ```ignore
//! let server = MockCdpServer::start(|method, _params, _sid| match method {
//!     "Page.navigate" => vec![
//!         FrameOut::Response(json!({ "frameId": "f1" })),
//!         FrameOut::Event {
//!             method: "Page.frameStoppedLoading".into(),
//!             params: json!({ "frameId": "f1" }),
//!             session_id: Some("s1".into()),
//!         },
//!     ],
//!     _ => vec![FrameOut::Response(json!({}))],
//! }).await;
//! let cdp = CdpClient::connect(&server.ws_url()).await?;
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use async_tungstenite::tokio::accept_async;
use async_tungstenite::tungstenite::Message;
use futures::StreamExt;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// One frame the mock will send back to the client. The handler can
/// return many in order — typically one `Response` followed by any
/// number of `Event` frames that simulate Chrome firing on the
/// session.
pub(crate) enum FrameOut {
    /// Reply to the incoming request. `id` is filled in by the loop
    /// from the request that triggered the handler.
    Response(Value),
    /// Push an unsolicited event with the given method, params, and
    /// optional `sessionId`.
    Event {
        method: String,
        params: Value,
        session_id: Option<String>,
    },
}

/// One incoming JSON-RPC request the mock observed. Captured for
/// post-hoc assertions (e.g. "did we send `Network.setExtraHTTPHeaders`
/// with the expected map?"). `id` and `session_id` are exposed so
/// future tests can assert on JSON-RPC plumbing details, even though
/// today's tests only look at `method` / `params`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct RecordedRequest {
    pub(crate) id: u64,
    pub(crate) method: String,
    pub(crate) params: Value,
    pub(crate) session_id: Option<String>,
}

pub(crate) struct MockCdpServer {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<RecordedRequest>>>,
    _accept_task: JoinHandle<()>,
}

impl MockCdpServer {
    /// Bind on `127.0.0.1` (ephemeral port), spawn an accept loop, and
    /// return immediately. The first incoming WebSocket connection is
    /// driven by `handler`; subsequent connections also work the same
    /// way (one connection at a time per accept iteration).
    ///
    /// `handler` is `Fn` (not `FnMut`) — wrap shared state in
    /// `Arc<Mutex<…>>` if you need to mutate across calls.
    pub(crate) async fn start<H>(handler: H) -> Self
    where
        H: Fn(&str, &Value, Option<&str>) -> Vec<FrameOut> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let received = Arc::new(Mutex::new(Vec::<RecordedRequest>::new()));
        let handler = Arc::new(handler);

        let received_for_task = Arc::clone(&received);
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let Ok(ws) = accept_async(sock).await else {
                    continue;
                };
                let received = Arc::clone(&received_for_task);
                let handler = Arc::clone(&handler);
                tokio::spawn(handle_connection(ws, received, handler));
            }
        });

        Self {
            addr,
            received,
            _accept_task: accept_task,
        }
    }

    pub(crate) fn ws_url(&self) -> String {
        format!("ws://{}/", self.addr)
    }

    pub(crate) async fn received(&self) -> Vec<RecordedRequest> {
        self.received.lock().await.clone()
    }
}

async fn handle_connection<S, H>(
    ws: async_tungstenite::WebSocketStream<async_tungstenite::tokio::TokioAdapter<S>>,
    received: Arc<Mutex<Vec<RecordedRequest>>>,
    handler: Arc<H>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    H: Fn(&str, &Value, Option<&str>) -> Vec<FrameOut> + Send + Sync + 'static,
{
    let (mut sink, mut stream) = ws.split();
    while let Some(msg) = stream.next().await {
        let Ok(Message::Text(text)) = msg else {
            // Ping/Pong/Binary/Close — ignore or break.
            if matches!(msg, Ok(Message::Close(_))) || msg.is_err() {
                break;
            }
            continue;
        };
        let Ok(req): Result<Value, _> = serde_json::from_str(&text) else {
            continue;
        };
        let id = req.get("id").and_then(Value::as_u64).unwrap_or(0);
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let session_id = req
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);

        received.lock().await.push(RecordedRequest {
            id,
            method: method.clone(),
            params: params.clone(),
            session_id: session_id.clone(),
        });

        let frames = handler(&method, &params, session_id.as_deref());
        for frame in frames {
            let payload = match frame {
                FrameOut::Response(result) => {
                    let mut obj = serde_json::json!({ "id": id, "result": result });
                    if let Some(sid) = &session_id {
                        obj["sessionId"] = Value::String(sid.clone());
                    }
                    obj
                }
                FrameOut::Event {
                    method,
                    params,
                    session_id,
                } => {
                    let mut obj = serde_json::json!({ "method": method, "params": params });
                    if let Some(sid) = session_id {
                        obj["sessionId"] = Value::String(sid);
                    }
                    obj
                }
            };
            if sink
                .send(Message::Text(payload.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}
