use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header::AUTHORIZATION};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::Error;

pub const DEFAULT_PORT: u16 = 34965;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
}

impl Endpoint {
    pub fn url(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("ws://[{}]:{}", self.host, self.port)
        } else {
            format!("ws://{}:{}", self.host, self.port)
        }
    }
}

/// A server-pushed event: a JSON-RPC notification.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub method: String,
    pub params: Value,
}

impl Event {
    pub fn to_json(&self) -> Value {
        json!({ "event": self.method, "params": self.params })
    }
}

type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type Reply = Result<Value, Error>;

#[derive(Default)]
struct Pending {
    replies: HashMap<u64, oneshot::Sender<Reply>>,
    closed: bool,
}

/// One WebSocket connection to the VCam API.
pub struct Connection {
    sink: tokio::sync::Mutex<Sink>,
    pending: std::sync::Arc<Mutex<Pending>>,
    next_id: AtomicU64,
    events: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
}

impl Connection {
    pub async fn connect(endpoint: &Endpoint) -> Result<Self, Error> {
        let connect_error = |reason: String| Error::Connect {
            url: endpoint.url(),
            reason,
        };
        let mut request = endpoint
            .url()
            .into_client_request()
            .map_err(|e| connect_error(e.to_string()))?;
        if let Some(token) = &endpoint.token {
            let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                Error::Usage(
                    "VCAM_TOKEN contains characters that cannot be sent in a header".into(),
                )
            })?;
            request.headers_mut().insert(AUTHORIZATION, value);
        }
        let (stream, _) =
            match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
                .await
            {
                Err(_) => return Err(connect_error("timed out".into())),
                Ok(Err(tungstenite::Error::Http(response))) => {
                    return Err(Error::Rejected {
                        url: endpoint.url(),
                        status: response.status().as_u16(),
                    });
                }
                Ok(Err(e)) => return Err(connect_error(e.to_string())),
                Ok(Ok(connected)) => connected,
            };
        let (sink, mut source) = stream.split();
        let pending = std::sync::Arc::new(Mutex::new(Pending::default()));
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        let reader_pending = pending.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = source.next().await {
                match message {
                    Message::Text(text) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            dispatch(value, &reader_pending, &event_sender);
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            // Calls such as motion.play with `wait` never time out, so a lost connection
            // must fail them or they would wait forever.
            let mut pending = reader_pending.lock().unwrap();
            pending.closed = true;
            for (_, reply) in pending.replies.drain() {
                let _ = reply.send(Err(Error::Closed));
            }
        });

        Ok(Self {
            sink: tokio::sync::Mutex::new(sink),
            pending,
            next_id: AtomicU64::new(1),
            events: Mutex::new(Some(event_receiver)),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.pending.lock().unwrap().closed
    }

    pub async fn call(&self, method: &str, params: Map<String, Value>) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.closed {
                return Err(Error::Closed);
            }
            pending.replies.insert(id, sender);
        }
        let mut request = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if !params.is_empty() {
            request["params"] = Value::Object(params);
        }
        if self.send(Message::text(request.to_string())).await.is_err() {
            self.pending.lock().unwrap().replies.remove(&id);
            return Err(Error::Closed);
        }
        receiver.await.unwrap_or(Err(Error::Closed))
    }

    pub async fn send_binary(&self, data: Vec<u8>) -> Result<(), Error> {
        self.send(Message::binary(data))
            .await
            .map_err(|_| Error::Closed)
    }

    /// Hands out the event stream. Only the first caller receives it.
    pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<Event>> {
        self.events.lock().unwrap().take()
    }

    async fn send(&self, message: Message) -> Result<(), tungstenite::Error> {
        self.sink.lock().await.send(message).await
    }
}

fn dispatch(value: Value, pending: &Mutex<Pending>, events: &mpsc::UnboundedSender<Event>) {
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        let reply = match value.get("error") {
            Some(error) => Err(Error::from_rpc(error)),
            None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        };
        if let Some(sender) = pending.lock().unwrap().replies.remove(&id) {
            let _ = sender.send(reply);
        }
    } else if let Some(method) = value.get("method").and_then(Value::as_str) {
        let _ = events.send(Event {
            method: method.to_owned(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_ipv6_hosts() {
        let endpoint = |host: &str| Endpoint {
            host: host.into(),
            port: 1,
            token: None,
        };
        assert_eq!(endpoint("127.0.0.1").url(), "ws://127.0.0.1:1");
        assert_eq!(endpoint("::1").url(), "ws://[::1]:1");
        assert_eq!(endpoint("[::1]").url(), "ws://[::1]:1");
    }

    async fn listener() -> (tokio::net::TcpListener, Endpoint) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            token: Some("secret".into()),
        };
        (listener, endpoint)
    }

    #[tokio::test]
    async fn sends_the_token_and_reports_a_refusal() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (listener, endpoint) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let length = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8_lossy(&request[..length]).to_ascii_lowercase()
        });
        let outcome = Connection::connect(&endpoint).await;
        assert!(matches!(outcome, Err(Error::Rejected { status: 400, .. })));
        let request = server.await.unwrap();
        assert!(request.contains("authorization: bearer secret"));
        assert!(
            !request.contains("origin:"),
            "VCam refuses handshakes with an Origin"
        );
    }

    #[tokio::test]
    async fn fails_waiting_calls_when_the_connection_drops() {
        let (listener, endpoint) = listener().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await;
            socket.close(None).await.unwrap();
        });
        let connection = Connection::connect(&endpoint).await.unwrap();
        let outcome = connection.call("motion.play", Map::new()).await;
        assert!(matches!(outcome, Err(Error::Closed)));
        assert!(connection.is_closed());
    }
}
