use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::spec::{Method, Spec, schema_ref_name};

/// `frame.capture` images go back to the model, so they are kept small unless asked otherwise.
pub const DEFAULT_CAPTURE_WIDTH: u64 = 960;
pub const DEFAULT_EVENT_WAIT_SECONDS: f64 = 10.0;
pub const EVENTS_WAIT: &str = "events_wait";

#[derive(Clone, Debug, PartialEq)]
pub enum ToolKind {
    Method(String),
    /// The prefix of an upload flow, e.g. "avatar.import"
    Upload(String),
    EventsWait,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub kind: ToolKind,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<Value>,
}

impl ToolDefinition {
    pub fn to_json(&self) -> Value {
        let mut tool = json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        });
        if let Some(annotations) = &self.annotations {
            tool["annotations"] = annotations.clone();
        }
        tool
    }
}

/// Tool names use `_` because some MCP clients reject dots
pub fn tool_name(method: &str) -> String {
    method.replace('.', "_")
}

/// One tool per method. `prefixes` keeps only methods in those namespaces, e.g. `scene` or `object`.
pub fn tool_definitions(spec: &Spec, prefixes: &[String]) -> Vec<ToolDefinition> {
    let included = |name: &str| {
        let namespace = name.split('.').next().unwrap_or(name);
        prefixes.is_empty() || prefixes.iter().any(|p| p == namespace)
    };
    let uploads = spec.upload_prefixes();
    let mut tools = Vec::new();
    for method in spec.methods() {
        let name = method.name();
        // The server itself subscribes to events and reads the spec, and uploads become one tool each
        let hidden = name == "rpc.discover"
            || name == "events.subscribe"
            || uploads.iter().any(|p| name.starts_with(&format!("{p}.")));
        if hidden || !included(name) {
            continue;
        }
        tools.push(method_tool(spec, method));
    }
    for prefix in uploads {
        if included(prefix) {
            tools.push(upload_tool(spec, prefix));
        }
    }
    if included("events") {
        tools.push(events_wait_tool(spec));
    }
    tools
}

fn method_tool(spec: &Spec, method: Method<'_>) -> ToolDefinition {
    let mut description = describe(method);
    if method.name() == "frame.capture" {
        description.push_str(&format!(
            "\n\nReturns the image. maxWidth defaults to {DEFAULT_CAPTURE_WIDTH} here to keep it small."
        ));
    }
    ToolDefinition {
        name: tool_name(method.name()),
        kind: ToolKind::Method(method.name().to_owned()),
        description,
        input_schema: input_schema(spec, method, &[], Map::new(), Vec::new()),
        annotations: annotations(method.effect()),
    }
}

fn upload_tool(spec: &Spec, prefix: &str) -> ToolDefinition {
    let commit = spec
        .method(&format!("{prefix}.commit"))
        .expect("upload prefixes have a commit method");
    let mut path = Map::new();
    path.insert(
        "path".into(),
        json!({ "type": "string", "description": "Path to the local file to upload. The file name is kept." }),
    );
    let begin_description = spec
        .method(&format!("{prefix}.begin"))
        .and_then(|m| m.summary())
        .unwrap_or_default();
    ToolDefinition {
        name: tool_name(prefix),
        kind: ToolKind::Upload(prefix.to_owned()),
        description: format!(
            "{} Uploads the local file at `path`. ({begin_description})",
            describe(commit)
        ),
        input_schema: input_schema(spec, commit, &["importId"], path, vec!["path".into()]),
        annotations: annotations(commit.effect()),
    }
}

fn events_wait_tool(spec: &Spec) -> ToolDefinition {
    let names: Vec<&str> = spec.events().map(|e| e.name()).collect();
    ToolDefinition {
        name: EVENTS_WAIT.into(),
        kind: ToolKind::EventsWait,
        description: format!(
            "Returns the VCam events received since the last call, waiting for one when there are none. \
             Most calls already answer when their change has finished, so this is for changes made by \
             the user or by tracking. Events: {}",
            names.join(", ")
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "events": {
                    "type": "array",
                    "items": { "type": "string", "enum": names },
                    "description": "Returns only these events. Others received in the meantime are dropped."
                },
                "timeoutSeconds": {
                    "type": "number",
                    "default": DEFAULT_EVENT_WAIT_SECONDS,
                    "description": "How long to wait when no event has arrived. Returns an empty list after that."
                }
            }
        }),
        annotations: Some(json!({ "readOnlyHint": true })),
    }
}

fn describe(method: Method<'_>) -> String {
    match (method.summary(), method.description()) {
        (Some(summary), Some(description)) => format!("{summary}. {description}"),
        (Some(text), None) | (None, Some(text)) => text.to_owned(),
        (None, None) => method.name().to_owned(),
    }
}

/// Without a stated effect the tool gets no hints, so clients fall back to the MCP default
/// of treating it as possibly destructive.
fn annotations(effect: Option<&str>) -> Option<Value> {
    match effect? {
        "read" => Some(json!({ "readOnlyHint": true })),
        "write" => Some(json!({ "readOnlyHint": false, "destructiveHint": false })),
        "destructive" => Some(json!({ "readOnlyHint": false, "destructiveHint": true })),
        _ => None,
    }
}

fn input_schema(
    spec: &Spec,
    method: Method<'_>,
    skip: &[&str],
    mut properties: Map<String, Value>,
    mut required: Vec<String>,
) -> Value {
    for param in method.params() {
        if skip.contains(&param.name) {
            continue;
        }
        let mut schema = rewrite_refs(param.schema.clone());
        if let (Some(description), Some(object)) = (param.description, schema.as_object_mut()) {
            object.insert("description".into(), json!(description));
        }
        properties.insert(param.name.to_owned(), schema);
        if param.required {
            required.push(param.name.to_owned());
        }
    }
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    let definitions = referenced_definitions(spec, &schema);
    if !definitions.is_empty() {
        schema["$defs"] = Value::Object(definitions);
    }
    schema
}

/// Only the shared schemas this tool reaches, so each tool stays small
fn referenced_definitions(spec: &Spec, schema: &Value) -> Map<String, Value> {
    let Some(schemas) = spec.schemas() else {
        return Map::new();
    };
    let mut seen = BTreeSet::new();
    let mut queue = Vec::new();
    collect_ref_names(schema, &mut queue);
    while let Some(name) = queue.pop() {
        if seen.insert(name.clone())
            && let Some(definition) = schemas.get(&name)
        {
            collect_ref_names(definition, &mut queue);
        }
    }
    seen.into_iter()
        .filter_map(|name| {
            let definition = schemas.get(&name)?.clone();
            Some((name, rewrite_refs(definition)))
        })
        .collect()
}

fn collect_ref_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(name) = schema_ref_name(value).or_else(|| definition_ref_name(value)) {
                names.push(name.to_owned());
            }
            object.values().for_each(|v| collect_ref_names(v, names));
        }
        Value::Array(array) => array.iter().for_each(|v| collect_ref_names(v, names)),
        _ => {}
    }
}

fn definition_ref_name(schema: &Value) -> Option<&str> {
    schema.get("$ref")?.as_str()?.strip_prefix("#/$defs/")
}

fn rewrite_refs(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| match (key.as_str(), value.as_str()) {
                    ("$ref", Some(target)) => {
                        let target = target.replace("#/components/schemas/", "#/$defs/");
                        (key, Value::String(target))
                    }
                    _ => (key, rewrite_refs(value)),
                })
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.into_iter().map(rewrite_refs).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(prefixes: &[&str]) -> Vec<ToolDefinition> {
        let prefixes: Vec<String> = prefixes.iter().map(|s| s.to_string()).collect();
        tool_definitions(&Spec::bundled(), &prefixes)
    }

    fn find<'a>(tools: &'a [ToolDefinition], name: &str) -> &'a ToolDefinition {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} is missing"))
    }

    #[test]
    fn replaces_upload_steps_with_one_tool() {
        let tools = tools(&[]);
        assert!(tools.iter().all(|t| !t.name.contains('.')));
        assert!(!tools.iter().any(|t| t.name.starts_with("avatar_import_")));
        assert!(
            !tools
                .iter()
                .any(|t| t.name == "rpc_discover" || t.name == "events_subscribe")
        );
        let upload = find(&tools, "avatar_import");
        assert_eq!(upload.kind, ToolKind::Upload("avatar.import".into()));
        assert_eq!(upload.input_schema["required"], json!(["path"]));
        assert!(upload.input_schema["properties"].get("importId").is_none());
        assert!(upload.input_schema["properties"].get("load").is_some());
    }

    #[test]
    fn carries_only_the_reachable_shared_schemas() {
        let tools = tools(&[]);
        let add = find(&tools, "text_add");
        let definitions = add.input_schema["$defs"].as_object().unwrap();
        assert!(definitions.contains_key("TextStyle"));
        assert!(
            definitions.contains_key("TextOutline"),
            "nested references are followed"
        );
        assert!(!definitions.contains_key("Avatar"));
        assert!(!add.input_schema.to_string().contains("#/components/"));
        assert!(
            find(&tools, "scene_list")
                .input_schema
                .get("$defs")
                .is_none()
        );
    }

    #[test]
    fn filters_by_namespace() {
        let names: Vec<String> = tools(&["scene", "events"])
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(names.contains(&"scene_load".to_owned()));
        assert!(names.contains(&EVENTS_WAIT.to_owned()));
        assert!(!names.contains(&"object_list".to_owned()));
        let names: Vec<String> = tools(&["image"]).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["image_import"]);
    }

    #[test]
    fn maps_effects_to_hints() {
        assert_eq!(
            annotations(Some("read")),
            Some(json!({ "readOnlyHint": true }))
        );
        assert_eq!(
            annotations(Some("destructive")),
            Some(json!({ "readOnlyHint": false, "destructiveHint": true }))
        );
        assert_eq!(annotations(None), None);
    }
}
