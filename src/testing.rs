use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

type Handler = dyn Fn(&str, &Value) -> Result<Value, Value> + Send + Sync;

#[derive(Default)]
struct Log {
    requests: Vec<(String, Value)>,
    binary_frames: Vec<Vec<u8>>,
}

/// A WebSocket server that answers JSON-RPC requests with `handler` and records what it receives
pub struct FakeServer {
    pub port: u16,
    log: Arc<Mutex<Log>>,
    pushes: mpsc::UnboundedSender<Value>,
}

impl FakeServer {
    pub async fn start(
        handler: impl Fn(&str, &Value) -> Result<Value, Value> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Log::default()));
        let handler: Arc<Handler> = Arc::new(handler);
        let (pushes, mut push_receiver) = mpsc::unbounded_channel::<Value>();
        let server_log = log.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut sink, mut source) = socket.split();
            loop {
                tokio::select! {
                    Some(push) = push_receiver.recv() => {
                        if sink.send(Message::text(push.to_string())).await.is_err() { break }
                    }
                    message = source.next() => {
                        let Some(Ok(message)) = message else { break };
                        match message {
                            Message::Text(text) => {
                                let request: Value = serde_json::from_str(&text).unwrap();
                                let method = request["method"].as_str().unwrap().to_owned();
                                let params = request.get("params").cloned().unwrap_or(json!({}));
                                server_log.lock().unwrap().requests.push((method.clone(), params.clone()));
                                let mut response = json!({ "jsonrpc": "2.0", "id": request["id"] });
                                match handler(&method, &params) {
                                    Ok(result) => response["result"] = result,
                                    Err(error) => response["error"] = error,
                                }
                                if sink.send(Message::text(response.to_string())).await.is_err() { break }
                            }
                            Message::Binary(data) => server_log.lock().unwrap().binary_frames.push(data.to_vec()),
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                }
            }
        });
        Self { port, log, pushes }
    }

    pub fn requests(&self) -> Vec<(String, Value)> {
        self.log.lock().unwrap().requests.clone()
    }

    pub fn binary_frames(&self) -> Vec<Vec<u8>> {
        self.log.lock().unwrap().binary_frames.clone()
    }

    pub fn push_event(&self, method: &str, params: Value) {
        let _ = self
            .pushes
            .send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }
}
