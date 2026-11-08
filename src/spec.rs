use serde_json::{Map, Value};

const BUNDLED: &str = include_str!("../spec/openrpc.json");
const SCHEMA_REF_PREFIX: &str = "#/components/schemas/";

/// The OpenRPC document of the VCam API, as served by `rpc.discover`.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    document: Value,
}

#[derive(Clone, Copy, Debug)]
pub struct Method<'a> {
    raw: &'a Value,
}

#[derive(Clone, Copy, Debug)]
pub struct Param<'a> {
    pub name: &'a str,
    pub required: bool,
    pub schema: &'a Value,
    pub description: Option<&'a str>,
    /// The VCam API version that added the parameter, when it came after the method
    pub since: Option<&'a str>,
}

impl Spec {
    /// The copy taken at build time, used when VCam is not running
    pub fn bundled() -> Self {
        Self::new(serde_json::from_str(BUNDLED).expect("the bundled openrpc.json is valid JSON"))
    }

    pub fn new(document: Value) -> Self {
        Self { document }
    }

    pub fn version(&self) -> &str {
        self.document
            .pointer("/info/version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    }

    fn all_methods(&self) -> impl Iterator<Item = Method<'_>> {
        self.document
            .get("methods")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|raw| Method { raw })
    }

    /// Methods a client can call, without the server-pushed events
    pub fn methods(&self) -> impl Iterator<Item = Method<'_>> {
        self.all_methods().filter(|m| !m.is_event())
    }

    pub fn events(&self) -> impl Iterator<Item = Method<'_>> {
        self.all_methods().filter(|m| m.is_event())
    }

    pub fn method(&self, name: &str) -> Option<Method<'_>> {
        self.methods().find(|m| m.name() == name)
    }

    pub fn schemas(&self) -> Option<&Map<String, Value>> {
        self.document
            .pointer("/components/schemas")
            .and_then(Value::as_object)
    }

    /// Follows `$ref`s into `components/schemas`
    pub fn resolve<'a>(&'a self, schema: &'a Value) -> &'a Value {
        let mut current = schema;
        for _ in 0..16 {
            let Some(name) = schema_ref_name(current) else {
                break;
            };
            match self.schemas().and_then(|s| s.get(name)) {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }

    /// Names of the upload flows, e.g. "avatar.import" for avatar.import.begin/commit/cancel
    pub fn upload_prefixes(&self) -> Vec<&str> {
        self.methods()
            .filter_map(|m| m.name().strip_suffix(".begin"))
            .filter(|prefix| prefix.ends_with(".import"))
            .filter(|prefix| self.method(&format!("{prefix}.commit")).is_some())
            .collect()
    }
}

fn since(raw: &Value) -> Option<&str> {
    raw.get("x-vcam-since").and_then(Value::as_str)
}

pub fn schema_ref_name(schema: &Value) -> Option<&str> {
    schema
        .get("$ref")?
        .as_str()?
        .strip_prefix(SCHEMA_REF_PREFIX)
}

impl<'a> Method<'a> {
    pub fn name(&self) -> &'a str {
        self.raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn summary(&self) -> Option<&'a str> {
        self.raw.get("summary").and_then(Value::as_str)
    }

    pub fn description(&self) -> Option<&'a str> {
        self.raw.get("description").and_then(Value::as_str)
    }

    pub fn is_event(&self) -> bool {
        self.raw.get("x-direction").and_then(Value::as_str) == Some("serverToClient")
    }

    /// The VCam API version that added the method. Methods without it date from the first version.
    pub fn since(&self) -> Option<&'a str> {
        since(self.raw)
    }

    /// `read`, `write`, or `destructive`, when the spec states it
    pub fn effect(&self) -> Option<&'a str> {
        self.raw.get("x-vcam-effect").and_then(Value::as_str)
    }

    pub fn params(&self) -> Vec<Param<'a>> {
        self.raw
            .get("params")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|raw| Param {
                name: raw.get("name").and_then(Value::as_str).unwrap_or_default(),
                required: raw
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                schema: raw.get("schema").unwrap_or(&Value::Null),
                description: raw.get("description").and_then(Value::as_str),
                since: since(raw),
            })
            .collect()
    }

    pub fn param(&self, name: &str) -> Option<Param<'a>> {
        self.params().into_iter().find(|p| p.name == name)
    }

    pub fn result_schema(&self) -> Option<&'a Value> {
        self.raw.pointer("/result/schema")
    }
}

/// A short label for a schema such as `integer`, `"png"|"jpeg"`, or `ObjectPlacement`
pub fn type_label(spec: &Spec, schema: &Value) -> String {
    if let Some(name) = schema_ref_name(schema) {
        let resolved = spec.resolve(schema);
        if let Some(values) = resolved.get("enum").and_then(Value::as_array) {
            return enum_label(values);
        }
        return name.to_owned();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return enum_label(values);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("array") => {
            let item = schema
                .get("items")
                .map(|items| type_label(spec, items))
                .unwrap_or_else(|| "any".into());
            format!("{item}[]")
        }
        Some(kind) => kind.to_owned(),
        None => "json".to_owned(),
    }
}

fn enum_label(values: &[Value]) -> String {
    values
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_events_from_methods() {
        let spec = Spec::bundled();
        assert!(spec.method("scene.load").is_some());
        assert!(spec.method("motion.started").is_none());
        assert!(spec.events().any(|e| e.name() == "motion.started"));
    }

    #[test]
    fn finds_upload_flows() {
        let spec = Spec::bundled();
        let prefixes = spec.upload_prefixes();
        for prefix in [
            "avatar.import",
            "image.import",
            "video.import",
            "motion.import",
        ] {
            assert!(
                prefixes.contains(&prefix),
                "{prefix} is missing from {prefixes:?}"
            );
        }
    }

    #[test]
    fn labels_types() {
        let spec = Spec::bundled();
        let capture = spec.method("frame.capture").unwrap();
        assert_eq!(
            type_label(&spec, capture.param("format").unwrap().schema),
            "png|jpeg"
        );
        assert_eq!(
            type_label(&spec, capture.param("maxWidth").unwrap().schema),
            "integer"
        );
    }
}
