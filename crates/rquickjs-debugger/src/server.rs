use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::session::{Inbound, Outbound, Target};
use crate::url::script_url;

const BIND_ADDRESS: &str = "127.0.0.1";

struct Entry {
    title: String,
    url: String,
    inbox: Sender<Inbound>,
}

pub struct Registry {
    targets: Mutex<HashMap<String, Entry>>,
    wake: Box<dyn Fn() + Send + Sync>,
    port: u16,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Registry {
    pub fn new(port: u16, wake: impl Fn() + Send + Sync + 'static) -> Arc<Registry> {
        Arc::new(Registry {
            targets: Mutex::new(HashMap::new()),
            wake: Box::new(wake),
            port,
        })
    }

    pub fn start(port: u16, wake: impl Fn() + Send + Sync + 'static) -> Arc<Registry> {
        let registry = Registry::new(port, wake);
        tokio::spawn(serve(registry.clone()));
        registry
    }

    pub fn register(&self, id: &str, title: &str, url: &str) -> Receiver<Inbound> {
        let (inbox, receiver) = std::sync::mpsc::channel();
        let entry = Entry {
            title: title.to_string(),
            url: url.to_string(),
            inbox,
        };
        self.targets().insert(id.to_string(), entry);
        receiver
    }

    pub fn unregister(&self, id: &str) {
        self.targets().remove(id);
    }

    fn targets(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.targets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn deliver(&self, id: &str, message: Inbound) -> bool {
        let delivered = self
            .targets()
            .get(id)
            .is_some_and(|t| t.inbox.send(message).is_ok());
        if delivered {
            (self.wake)();
        }
        delivered
    }
}

#[derive(Debug)]
pub struct Targets {
    registry: Arc<Registry>,
    targets: HashMap<String, Target>,
}

impl Targets {
    pub fn new(registry: Arc<Registry>) -> Targets {
        Targets {
            registry,
            targets: HashMap::new(),
        }
    }

    pub fn target(&mut self, name: &str, title: &str, main: &str) -> Target {
        self.targets
            .entry(name.to_string())
            .or_insert_with(|| {
                let inbox = self
                    .registry
                    .register(&target_id(name), title, &script_url(main));
                Target::new(name, inbox)
            })
            .clone()
    }

    pub fn forget(&mut self, name: &str) {
        if self.targets.remove(name).is_some() {
            self.registry.unregister(&target_id(name));
        }
    }

    pub fn poll(&self) {
        for target in self.targets.values() {
            target.poll();
        }
    }
}

#[must_use]
pub fn target_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub async fn serve(registry: Arc<Registry>) {
    let port = registry.port;
    let app = axum::Router::new()
        .route("/json", get(list))
        .route("/json/list", get(list))
        .route("/json/version", get(version))
        .route("/{id}", get(attach))
        .with_state(registry);
    let listener = match tokio::net::TcpListener::bind((BIND_ADDRESS, port)).await {
        Ok(listener) => listener,
        Err(error) => {
            warn!("debugger: cannot listen on {BIND_ADDRESS}:{port}: {error}");
            return;
        }
    };
    info!("debugger listening on {BIND_ADDRESS}:{port} (Chrome DevTools Protocol)");
    if let Err(error) = axum::serve(listener, app).await {
        warn!("debugger stopped: {error}");
    }
}

async fn version(State(registry): State<Arc<Registry>>) -> Json<serde_json::Value> {
    Json(json!({
        "Browser": format!("rquickjs-debugger/{}", env!("CARGO_PKG_VERSION")),
        "Protocol-Version": "1.3",
        "webSocketDebuggerUrl": format!("ws://{BIND_ADDRESS}:{}/", registry.port),
    }))
}

async fn list(State(registry): State<Arc<Registry>>) -> Json<serde_json::Value> {
    let port = registry.port;
    let mut targets: Vec<serde_json::Value> = registry
        .targets()
        .iter()
        .map(|(id, target)| {
            let socket = format!("{BIND_ADDRESS}:{port}/{id}");
            json!({
                "id": id,
                "type": "node",
                "title": target.title,
                "description": "JavaScript runtime",
                "url": target.url,
                "webSocketDebuggerUrl": format!("ws://{socket}"),
                "devtoolsFrontendUrl": format!(
                    "devtools://devtools/bundled/js_app.html?experiments=true&v8only=true&ws={socket}"
                ),
            })
        })
        .collect();
    targets.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
    Json(serde_json::Value::Array(targets))
}

async fn attach(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> Response {
    if !registry.targets().contains_key(&id) {
        return (StatusCode::NOT_FOUND, "no such target").into_response();
    }
    ws.on_upgrade(move |socket| client_socket(socket, id, registry))
}

async fn client_socket(mut socket: WebSocket, id: String, registry: Arc<Registry>) {
    let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let outbound: Outbound = Box::new(move |text| {
        let _ = outbound_tx.send(text);
    });
    if !registry.deliver(&id, Inbound::Connected(outbound)) {
        return;
    }
    debug!("debugger: client attached to '{id}'");
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if !registry.deliver(&id, Inbound::Message(text.to_string())) {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            outgoing = outbound_rx.recv() => match outgoing {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    registry.deliver(&id, Inbound::Disconnected);
    debug!("debugger: client left '{id}'");
}
