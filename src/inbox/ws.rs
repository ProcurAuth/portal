//! The WebSocket of the phones, over a hyper connection.
//!
//! The function [`upgrade`] checks the handshake and answers `101`. Then it gives the upgraded
//! connection to the task of the caller as a [`Socket`]. The framing comes from `tungstenite`.

use std::future::Future;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONNECTION, UPGRADE};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::WebSocketStream;

pub use tokio_tungstenite::tungstenite::Message;

/// The time that a close frame has to leave before the inbox gives up.
const CLOSE_SEND_LIMIT: Duration = Duration::from_secs(2);

/// An open WebSocket.
pub struct Socket {
    inner: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
}

impl Socket {
    /// Sends one message. An error means that the connection is closed.
    pub async fn send(&mut self, message: Message) -> Result<(), ()> {
        self.inner.send(message).await.map_err(|_| ())
    }

    /// The next message. `None` means that the stream ended. An error means that the frame or
    /// the connection is bad.
    pub async fn recv(&mut self) -> Option<Result<Message, ()>> {
        self.inner.next().await.map(|r| r.map_err(|_| ()))
    }

    /// Sends a close frame with a code and a reason. The call does not wait for the peer.
    pub async fn close_with(&mut self, code: u16, reason: &str) {
        let frame = CloseFrame {
            code: code.into(),
            reason: reason.to_string().into(),
        };
        let _ = tokio::time::timeout(
            CLOSE_SEND_LIMIT,
            self.inner.send(Message::Close(Some(frame))),
        )
        .await;
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())));
    *response.status_mut() = status;
    response
}

/// Tells whether a header holds a token. The header can hold a list.
fn has_token(request: &Request<Incoming>, name: hyper::header::HeaderName, token: &str) -> bool {
    request.headers().get_all(name).iter().any(|value| {
        value
            .to_str()
            .map(|text| {
                text.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case(token))
            })
            .unwrap_or(false)
    })
}

/// Checks the handshake of a request. On success, `session` runs on the upgraded connection and
/// the result is the answer `101`. On failure, the result is the answer to send.
#[allow(clippy::result_large_err)] // both sides are an answer
pub fn upgrade<F, Fut>(
    mut request: Request<Incoming>,
    max_message: usize,
    session: F,
) -> Result<Response<Full<Bytes>>, Response<Full<Bytes>>>
where
    F: FnOnce(Socket) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    if request.method() != Method::GET {
        return Err(plain(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed\n",
        ));
    }
    if !has_token(&request, CONNECTION, "upgrade") {
        return Err(plain(
            StatusCode::BAD_REQUEST,
            "expected a WebSocket upgrade\n",
        ));
    }
    if !has_token(&request, UPGRADE, "websocket") {
        return Err(plain(
            StatusCode::BAD_REQUEST,
            "expected a WebSocket upgrade\n",
        ));
    }
    if request
        .headers()
        .get("sec-websocket-version")
        .map(HeaderValue::as_bytes)
        != Some(b"13")
    {
        return Err(plain(
            StatusCode::BAD_REQUEST,
            "the WebSocket version must be 13\n",
        ));
    }
    let Some(key) = request.headers().get("sec-websocket-key") else {
        return Err(plain(
            StatusCode::BAD_REQUEST,
            "the WebSocket key is missing\n",
        ));
    };
    let accept = derive_accept_key(key.as_bytes());
    let on_upgrade = hyper::upgrade::on(&mut request);
    let config = WebSocketConfig::default()
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message));
    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let inner =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config))
                .await;
        session(Socket { inner }).await;
    });
    let mut response = plain(StatusCode::SWITCHING_PROTOCOLS, "");
    let headers = response.headers_mut();
    headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    if let Ok(value) = HeaderValue::from_str(&accept) {
        headers.insert("sec-websocket-accept", value);
    }
    Ok(response)
}
