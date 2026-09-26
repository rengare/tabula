//! WebSocket transport: serves the embedded web client and carries one
//! protocol message per binary WebSocket message.

use anyhow::Result;
use axum::Router;
use axum::extract::ws::{self, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::sync::Arc;
use tabula_protocol::{DecodeError, Message};
use tabula_session::{MessageSink, MessageStream};

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct Assets;

/// Starts a session for each accepted WebSocket.
pub type SessionFactory = Arc<dyn Fn(WsSink, WsStream) + Send + Sync>;

#[derive(Clone)]
struct AppState {
    on_connect: SessionFactory,
    /// Required `?token=` on the WebSocket, for listeners reachable from the LAN.
    token: Option<Arc<str>>,
}

pub fn router(on_connect: SessionFactory, token: Option<Arc<str>>) -> Router {
    Router::new()
        .route("/", get(|| asset("index.html")))
        .route("/ws", get(upgrade))
        .route("/{*path}", get(|Path(p): Path<String>| asset(p)))
        .with_state(AppState { on_connect, token })
}

/// Compares without an early exit, so timing doesn't leak how much matched.
fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn asset(path: impl AsRef<str>) -> Response {
    let path = path.as_ref();
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_owned())], file.data).into_response()
        }
        None if path == "index.html" => (
            StatusCode::NOT_FOUND,
            "web client not built: run `npm install && npm run build` in web/, then rebuild tabula",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(token) = &state.token
        && !query.get("token").is_some_and(|t| same_token(t, token))
    {
        tracing::warn!("rejected a LAN connection without a valid token");
        return (StatusCode::FORBIDDEN, "missing or wrong pairing token").into_response();
    }
    let on_connect = state.on_connect;
    ws.on_upgrade(move |socket| async move {
        let (tx, rx) = socket.split();
        on_connect(WsSink(tx), WsStream(rx));
    })
}

pub struct WsSink(SplitSink<WebSocket, ws::Message>);
pub struct WsStream(SplitStream<WebSocket>);

impl MessageSink for WsSink {
    async fn send(&mut self, msg: Message) -> Result<()> {
        self.0.send(ws::Message::Binary(msg.encode().into())).await?;
        Ok(())
    }
}

impl MessageStream for WsStream {
    async fn recv(&mut self) -> Result<Option<Result<Message, DecodeError>>> {
        loop {
            match self.0.next().await {
                None | Some(Ok(ws::Message::Close(_))) => return Ok(None),
                Some(Ok(ws::Message::Binary(b))) => return Ok(Some(Message::decode(&b))),
                Some(Ok(_)) => continue, // text, ping, pong
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}
