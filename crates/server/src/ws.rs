//! WebSocket transport: serves the embedded web client and carries one
//! protocol message per binary WebSocket message.

use anyhow::Result;
use axum::Router;
use axum::extract::ws::{self, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rust_embed::RustEmbed;
use std::sync::Arc;
use tabula_protocol::{DecodeError, Message};
use tabula_session::{MessageSink, MessageStream};

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct Assets;

/// Starts a session for each accepted WebSocket.
pub type SessionFactory = Arc<dyn Fn(WsSink, WsStream) + Send + Sync>;

pub fn router(on_connect: SessionFactory) -> Router {
    Router::new()
        .route("/", get(|| asset("index.html")))
        .route("/ws", get(upgrade))
        .route("/{*path}", get(|Path(p): Path<String>| asset(p)))
        .with_state(on_connect)
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

async fn upgrade(ws: WebSocketUpgrade, State(on_connect): State<SessionFactory>) -> Response {
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
