//! Bidirectional HTTP/2 client for Cursor's `agent.v1.AgentService/Run`.
//!
//! Unlike the existing `cursor_protocol::send_cursor_request` path — which sends
//! one fixed-size protobuf and reads the response stream — `AgentService/Run`
//! is **client-streaming + server-streaming**. After the initial RunRequest
//! frame we may still need to write additional Connect-RPC frames (e.g.
//! `ExecClientMessage.RequestContextResult`, `McpResult`, `KvClient` blob
//! replies) while continuing to read server frames on the same h2 stream.
//!
//! This module sets up a hyper-1.x h2 client with a `StreamBody` whose stream
//! is fed by an mpsc channel, so the caller can `send_frame()` at any point
//! before closing the request side. The response body is parsed incrementally
//! through `ConnectFrameParser`.

use super::cursor_agent_proto::{ConnectFrame, ConnectFrameParser, ProtoError};
use crate::proxy::ProxyError;
use async_stream::stream;
use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http_body::Frame;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

const DEFAULT_API_BASE_URL: &str = "https://agentn.global.api5.cursor.sh";
const AGENT_PATH: &str = "/agent.v1.AgentService/Run";

/// Internal body stream item shape: data Frames carried by an mpsc channel.
type BodyStream = Pin<Box<dyn futures::Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>;

/// One opened HTTP/2 client-streaming request to Cursor's AgentService.
///
/// Holds a writer side (mpsc sender → request body) and a reader side
/// (`hyper::body::Incoming` → `ConnectFrameParser`). Drop closes the request
/// body channel, which signals end-of-client-stream to hyper.
pub struct CursorH2Stream {
    writer: UnboundedSender<Bytes>,
    response: hyper::Response<Incoming>,
    parser: ConnectFrameParser,
    trailers: Option<HeaderMap>,
    pending: std::collections::VecDeque<ConnectFrame>,
    closed: bool,
    received_any_frame: bool,
}

/// Default per-call deadline. The session manager arms its own idle timer,
/// so this is just a backstop against an upstream stall.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// Timeout for the *first* server frame. Cursor's AgentService should start
/// producing output within seconds; if it hasn't after this deadline the
/// upstream is almost certainly waiting for a client-stream EOF we never
/// sent. A short timeout here lets the caller emit a compliant terminal
/// error event instead of hanging for 600s.
const FIRST_FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

impl CursorH2Stream {
    /// Open a fresh h2 stream to Cursor's AgentService endpoint,
    /// write the first Connect-RPC frame containing the encoded RunRequest,
    /// and return the live stream handle. Additional frames can be written
    /// via [`send_frame`].
    pub async fn open(
        headers: Vec<(String, String)>,
        first_frame: Bytes,
    ) -> Result<Self, ProxyError> {
        let url = format!("{DEFAULT_API_BASE_URL}{AGENT_PATH}");
        let uri = url
            .parse::<http::Uri>()
            .map_err(|e| ProxyError::ForwardFailed(format!("解析 Cursor URL 失败: {e}")))?;

        // ALPN-negotiated h2 via hyper-rustls. The legacy ALB on cursor's
        // edge rejects h2-prior-knowledge with 464, so we advertise h2 via
        // ALPN and refuse HTTP/1.1 downgrades.
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_only()
            .enable_http2()
            .build();
        let mut builder = Client::builder(TokioExecutor::new());
        builder.http2_only(true);
        builder.http2_adaptive_window(true);

        let (tx, rx) = unbounded_channel::<Bytes>();
        let initial = first_frame;
        // Convert the mpsc receiver to a stream of body Frames. The initial
        // frame is enqueued before we await — guarantees the first byte hits
        // the wire as soon as hyper opens the stream.
        let _ = tx.send(initial);
        let body_stream: BodyStream = Box::pin(stream! {
            let mut rx = rx;
            while let Some(chunk) = rx.recv().await {
                yield Ok::<_, std::io::Error>(Frame::data(chunk));
            }
        });
        let body = BodyExt::boxed_unsync(StreamBody::new(body_stream));

        // Build the request with cursor-agent's actual headers (caller passes
        // identity headers — auth, machine id, checksum, content-type).
        let mut req = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .body(body)
            .map_err(|e| ProxyError::ForwardFailed(format!("创建 Cursor Agent 请求失败: {e}")))?;
        for (k, v) in &headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| ProxyError::ForwardFailed(format!("Cursor 请求头名称无效: {e}")))?;
            let value = HeaderValue::from_str(v)
                .map_err(|e| ProxyError::ForwardFailed(format!("Cursor 请求头值无效: {e}")))?;
            req.headers_mut().insert(name, value);
        }

        let client: Client<_, http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>> =
            builder.build(https);

        let response = tokio::time::timeout(DEFAULT_TIMEOUT, client.request(req))
            .await
            .map_err(|_| ProxyError::ForwardFailed("Cursor AgentService 请求超时".to_string()))?
            .map_err(|e| ProxyError::ForwardFailed(format!("Cursor AgentService 请求失败: {e}")))?;

        Ok(Self {
            writer: tx,
            response,
            parser: ConnectFrameParser::new(),
            trailers: None,
            pending: std::collections::VecDeque::new(),
            closed: false,
            received_any_frame: false,
        })
    }

    pub fn status(&self) -> http::StatusCode {
        self.response.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }

    /// Send a Connect-RPC framed payload on the request body. Returns Err if
    /// the writer has been dropped (i.e. the request body has been closed).
    pub fn send_frame(&self, frame: Bytes) -> Result<(), ProxyError> {
        self.writer.send(frame).map_err(|_| {
            ProxyError::ForwardFailed("Cursor h2 stream 已关闭，无法继续写入".to_string())
        })
    }

    /// Signal end-of-client-stream. After this, no more frames can be sent.
    pub fn close_writer(&mut self) {
        // Closing the channel happens implicitly when the sender is dropped,
        // but we expose this explicitly so the session manager can choose to
        // half-close while continuing to read.
        // Replace with an already-closed sender; this drops the live one.
        let (closed_tx, _) = unbounded_channel::<Bytes>();
        self.writer = closed_tx;
    }

    /// Pull the next decoded Connect-RPC frame from the response body. Returns
    /// `Ok(None)` when the response body has ended cleanly. Trailers are
    /// captured into `self.trailers` and don't surface as frames.
    pub async fn next_frame(&mut self) -> Result<Option<ConnectFrame>, ProxyError> {
        if let Some(frame) = self.pending.pop_front() {
            self.received_any_frame = true;
            return Ok(Some(frame));
        }
        if self.closed {
            return Ok(None);
        }

        loop {
            let timeout = if self.received_any_frame {
                DEFAULT_TIMEOUT
            } else {
                FIRST_FRAME_IDLE_TIMEOUT
            };
            let frame_result =
                tokio::time::timeout(timeout, self.response.body_mut().frame())
                    .await
                    .map_err(|_| {
                        if !self.received_any_frame {
                            ProxyError::ForwardFailed(
                                "Cursor AgentService 首帧超时：上游在 30s 内未返回任何帧，\
                                 可能需要 client-stream EOF 信号"
                                    .to_string(),
                            )
                        } else {
                            ProxyError::ForwardFailed("Cursor AgentService 响应超时".to_string())
                        }
                    })?;

            let body_frame = match frame_result {
                Some(Ok(f)) => f,
                Some(Err(e)) => {
                    return Err(ProxyError::ForwardFailed(format!(
                        "Cursor 响应流读取失败: {e}"
                    )));
                }
                None => {
                    self.closed = true;
                    return Ok(None);
                }
            };

            if body_frame.is_trailers() {
                if let Ok(t) = body_frame.into_trailers() {
                    self.trailers = Some(t);
                }
                continue;
            }
            if let Ok(data) = body_frame.into_data() {
                let new_frames = self.parser.feed(&data).map_err(map_proto_err)?;
                for f in new_frames {
                    self.pending.push_back(f);
                }
                if let Some(f) = self.pending.pop_front() {
                    self.received_any_frame = true;
                    return Ok(Some(f));
                }
                // Empty data frame — keep reading.
                continue;
            }
        }
    }

    /// Whether we have received at least one server frame on this stream.
    pub fn received_any_frame(&self) -> bool {
        self.received_any_frame
    }

    /// Trailers captured after the response body ended. `grpc-status` /
    /// `grpc-message` typically live here for Connect-RPC over h2.
    pub fn trailers(&self) -> Option<&HeaderMap> {
        self.trailers.as_ref()
    }

    /// Connect-RPC grpc-status code from trailers. `0` = OK.
    pub fn grpc_status(&self) -> Option<u32> {
        self.trailers
            .as_ref()
            .and_then(|t| t.get("grpc-status"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse().ok())
    }

    pub fn grpc_message(&self) -> Option<String> {
        self.trailers
            .as_ref()
            .and_then(|t| t.get("grpc-message"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }
}

fn map_proto_err(e: ProtoError) -> ProxyError {
    ProxyError::ForwardFailed(format!("Cursor Connect-RPC 解码失败: {e}"))
}

/// Drain whatever frames are already in the response body without blocking
/// for more. Returns immediately if no whole frame is currently available.
/// Useful for tests and for non-blocking polling.
#[cfg(test)]
pub async fn try_drain_one(stream: &mut CursorH2Stream) -> Option<ConnectFrame> {
    use futures::FutureExt;
    stream
        .next_frame()
        .now_or_never()
        .and_then(Result::ok)
        .flatten()
}

/// Shape of the headers cursor-agent sends on every `agent.v1` request.
/// Auth/machine/checksum specifics come from `cursor_identity_headers` in
/// `cursor_protocol.rs`; this helper just enforces the Connect-RPC content
/// type and protocol headers.
pub fn agent_connect_headers() -> Vec<(String, String)> {
    vec![
        (
            "content-type".to_string(),
            "application/connect+proto".to_string(),
        ),
        ("connect-protocol-version".to_string(), "1".to_string()),
        ("accept-encoding".to_string(), "gzip, br".to_string()),
        ("te".to_string(), "trailers".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_connect_headers_include_connect_protocol() {
        let hs = agent_connect_headers();
        assert!(hs
            .iter()
            .any(|(k, v)| k == "content-type" && v == "application/connect+proto"));
        assert!(hs.iter().any(|(k, _)| k == "connect-protocol-version"));
        assert!(hs.iter().any(|(k, v)| k == "te" && v == "trailers"));
    }
}
