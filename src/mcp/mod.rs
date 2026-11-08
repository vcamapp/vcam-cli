pub mod tools;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{Peer, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;

use crate::cli::session;
use crate::client::{Connection, Endpoint, Event};
use crate::error::Error;
use crate::spec::Spec;
use crate::upload::upload;
use tools::{
    DEFAULT_CAPTURE_WIDTH, DEFAULT_EVENT_WAIT_SECONDS, ToolDefinition, ToolKind, tool_definitions,
};

const EVENT_BUFFER_LIMIT: usize = 100;
const LIST_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

const INSTRUCTIONS: &str = "\
Controls VCam, the virtual camera app with an avatar, through the VCam API.
- Changes are saved to the user's scene. To experiment, run scene_duplicate, work in the copy, and load the original scene again when done.
- After placing or changing something, check how it looks with frame_capture.
- Object positions are fractions of the canvas measured from its center, with y pointing up.
- There are no tools to change VCam settings. When VCam is unreachable or frame_capture is not allowed, ask the user to change the VCam API settings.";

pub async fn serve(endpoint: Endpoint, prefixes: Vec<String>) -> Result<(), Error> {
    let server = VCamServer {
        state: Arc::new(State {
            endpoint,
            prefixes,
            connection: tokio::sync::Mutex::new(None),
            tools: RwLock::new(Vec::new()),
            events: Mutex::new(VecDeque::new()),
            event_arrived: Notify::new(),
            peer: Mutex::new(None),
        }),
    };
    server.state.set_spec(&Spec::bundled());
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| Error::Io(format!("Could not start the MCP server: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| Error::Io(format!("The MCP server stopped: {e}")))?;
    Ok(())
}

#[derive(Clone)]
struct VCamServer {
    state: Arc<State>,
}

struct State {
    endpoint: Endpoint,
    prefixes: Vec<String>,
    connection: tokio::sync::Mutex<Option<Arc<Connection>>>,
    tools: RwLock<Vec<ToolDefinition>>,
    events: Mutex<VecDeque<Event>>,
    event_arrived: Notify,
    peer: Mutex<Option<Peer<RoleServer>>>,
}

impl State {
    /// Returns true when the tools changed
    fn set_spec(&self, spec: &Spec) -> bool {
        let tools = tool_definitions(spec, &self.prefixes);
        let mut current = self.tools.write().unwrap();
        let changed = *current != tools;
        *current = tools;
        changed
    }

    /// Connects on first use and again after VCam restarts. Events sent while disconnected are lost.
    async fn connection(self: &Arc<Self>) -> Result<Arc<Connection>, Error> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref().filter(|c| !c.is_closed()) {
            return Ok(connection.clone());
        }
        let (connection, spec) = session(&self.endpoint).await?;
        let connection = Arc::new(connection);
        let mut receiver = connection
            .take_events()
            .expect("a new connection has its events");
        connection.call("events.subscribe", Map::new()).await?;
        let state = self.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let mut events = state.events.lock().unwrap();
                if events.len() == EVENT_BUFFER_LIMIT {
                    events.pop_front();
                }
                events.push_back(event);
                drop(events);
                state.event_arrived.notify_waiters();
            }
        });
        if self.set_spec(&spec) {
            let peer = self.peer.lock().unwrap().clone();
            if let Some(peer) = peer {
                let _ = peer.notify_tool_list_changed().await;
            }
        }
        *slot = Some(connection.clone());
        Ok(connection)
    }

    fn tool_kind(&self, name: &str) -> Option<ToolKind> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.kind.clone())
    }

    /// Keeps the client to tell when the tools change. Clients on protocol 2026-07-28 never
    /// send `initialized`, so the peer is taken from whichever request comes first.
    fn remember(&self, peer: &Peer<RoleServer>) {
        let mut slot = self.peer.lock().unwrap();
        if slot.is_none() {
            *slot = Some(peer.clone());
        }
    }

    async fn call(
        self: &Arc<Self>,
        kind: ToolKind,
        arguments: Map<String, Value>,
    ) -> Result<Vec<ContentBlock>, Error> {
        match kind {
            ToolKind::Method(method) if method == "frame.capture" => self.capture(arguments).await,
            ToolKind::Method(method) => {
                let result = self.connection().await?.call(&method, arguments).await?;
                Ok(vec![json_content(&result)])
            }
            ToolKind::Upload(prefix) => {
                let mut arguments = arguments;
                let path = match arguments.remove("path") {
                    Some(Value::String(path)) => path,
                    _ => return Err(Error::Usage("path is required".into())),
                };
                let connection = self.connection().await?;
                let result = upload(
                    &connection,
                    &prefix,
                    Path::new(&path),
                    arguments,
                    std::future::pending(),
                )
                .await?;
                Ok(vec![json_content(&result)])
            }
            ToolKind::EventsWait => {
                let filter: Option<Vec<String>> = arguments
                    .get("events")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let seconds = arguments
                    .get("timeoutSeconds")
                    .and_then(Value::as_f64)
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .unwrap_or(DEFAULT_EVENT_WAIT_SECONDS);
                self.connection().await?;
                let events = self
                    .wait_events(filter, Duration::from_secs_f64(seconds))
                    .await;
                let list: Vec<Value> = events.iter().map(Event::to_json).collect();
                Ok(vec![json_content(&Value::Array(list))])
            }
        }
    }

    async fn capture(
        self: &Arc<Self>,
        mut arguments: Map<String, Value>,
    ) -> Result<Vec<ContentBlock>, Error> {
        arguments
            .entry("maxWidth")
            .or_insert(json!(DEFAULT_CAPTURE_WIDTH));
        let mut result = self
            .connection()
            .await?
            .call("frame.capture", arguments)
            .await?;
        let data = result.as_object_mut().and_then(|r| r.remove("data"));
        let mime_type = result
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/jpeg")
            .to_owned();
        match data {
            Some(Value::String(data)) => Ok(vec![
                ContentBlock::image(data, mime_type),
                json_content(&result),
            ]),
            _ => Err(Error::Io("frame.capture returned no image".into())),
        }
    }

    async fn wait_events(&self, filter: Option<Vec<String>>, timeout: Duration) -> Vec<Event> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let arrived = self.event_arrived.notified();
            tokio::pin!(arrived);
            // Registers for the next notification before draining, so an event pushed in between is not missed
            arrived.as_mut().enable();
            let matched: Vec<Event> = self
                .events
                .lock()
                .unwrap()
                .drain(..)
                .filter(|e| {
                    filter
                        .as_ref()
                        .is_none_or(|names| names.contains(&e.method))
                })
                .collect();
            if !matched.is_empty() {
                return matched;
            }
            if tokio::time::timeout_at(deadline, arrived).await.is_err() {
                return Vec::new();
            }
        }
    }
}

fn json_content(value: &Value) -> ContentBlock {
    ContentBlock::text(value.to_string())
}

impl ServerHandler for VCamServer {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_instructions(INSTRUCTIONS);
        config.server_info =
            Implementation::new("vcam", env!("CARGO_PKG_VERSION")).with_title("VCam");
        config.server_info.website_url = Some("https://github.com/vcamapp/vcam-cli".into());
        config
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.state.remember(&context.peer);
        // Lists the running VCam's tools rather than the bundled ones whenever VCam is reachable.
        // The bound keeps a listing quick when VCam runs on a device that does not answer.
        let _ = tokio::time::timeout(LIST_CONNECT_TIMEOUT, self.state.connection()).await;
        let tools = self
            .state
            .tools
            .read()
            .unwrap()
            .iter()
            .map(|t| serde_json::from_value::<Tool>(t.to_json()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.state.remember(&context.peer);
        let Some(kind) = self.state.tool_kind(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("Unknown tool '{}'", request.name),
                None,
            ));
        };
        let arguments = request.arguments.unwrap_or_default();
        let result = match self.state.call(kind, arguments).await {
            Ok(content) => CallToolResult::success(content),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeServer;

    async fn state_with(server: &FakeServer) -> Arc<State> {
        let state = Arc::new(State {
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: server.port,
                token: None,
            },
            prefixes: Vec::new(),
            connection: tokio::sync::Mutex::new(None),
            tools: RwLock::new(Vec::new()),
            events: Mutex::new(VecDeque::new()),
            event_arrived: Notify::new(),
            peer: Mutex::new(None),
        });
        state.set_spec(&Spec::bundled());
        state
    }

    fn discover_and(
        handler: impl Fn(&str, &Value) -> Result<Value, Value> + Send + Sync + 'static,
    ) -> impl Fn(&str, &Value) -> Result<Value, Value> + Send + Sync + 'static {
        let document: Value =
            serde_json::from_str(include_str!("../../spec/openrpc.json")).unwrap();
        move |method, params| match method {
            "rpc.discover" => Ok(document.clone()),
            "events.subscribe" => Ok(json!(true)),
            _ => handler(method, params),
        }
    }

    #[tokio::test]
    async fn follows_the_tools_of_the_running_vcam() {
        let mut document: Value =
            serde_json::from_str(include_str!("../../spec/openrpc.json")).unwrap();
        document["methods"]
            .as_array_mut()
            .unwrap()
            .retain(|m| m["name"] != "emoji.show");
        let server = FakeServer::start(move |method, _| match method {
            "rpc.discover" => Ok(document.clone()),
            _ => Ok(json!(true)),
        })
        .await;
        let state = state_with(&server).await;
        assert!(state.tool_kind("emoji_show").is_some());
        state.connection().await.unwrap();
        assert!(state.tool_kind("emoji_show").is_none());
    }

    #[tokio::test]
    async fn returns_captures_as_small_images() {
        let server = FakeServer::start(discover_and(|_, params| {
            Ok(json!({ "data": "aGk=", "mimeType": "image/jpeg", "width": params["maxWidth"], "height": 540 }))
        }))
        .await;
        let state = state_with(&server).await;
        let content = state
            .call(state.tool_kind("frame_capture").unwrap(), Map::new())
            .await
            .unwrap();
        assert!(matches!(&content[0], ContentBlock::Image(_)));
        let capture = server
            .requests()
            .into_iter()
            .find(|(m, _)| m == "frame.capture")
            .unwrap();
        assert_eq!(capture.1, json!({ "maxWidth": DEFAULT_CAPTURE_WIDTH }));
    }

    #[tokio::test]
    async fn hands_out_buffered_events_by_name() {
        let server = FakeServer::start(discover_and(|_, _| Ok(json!({})))).await;
        let state = state_with(&server).await;
        state.connection().await.unwrap();
        server.push_event("motion.started", json!({ "motionId": "builtin:wave" }));
        server.push_event("motion.stopped", json!({ "motionId": "builtin:wave" }));

        let filter = Some(vec!["motion.stopped".to_owned()]);
        let events = state.wait_events(filter, Duration::from_secs(5)).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].method, "motion.stopped");
        assert!(
            state
                .wait_events(None, Duration::from_millis(50))
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reports_vcam_errors_as_text() {
        let server = FakeServer::start(discover_and(|_, _| {
            Err(json!({ "code": 1003, "message": "Scene was not found.", "data": { "code": "scene_not_found" } }))
        }))
        .await;
        let state = state_with(&server).await;
        let mut arguments = Map::new();
        arguments.insert("sceneId".into(), json!(99));
        let error = state
            .call(state.tool_kind("scene_load").unwrap(), arguments)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Scene was not found. (scene_not_found)");
    }
}
