//! Running VCam apps may be older than this tool, since VCam is bought per version and people
//! keep using the version they have. These checks tell them that a method, parameter, or event
//! comes with a newer VCam API, rather than reporting it like a typo.

use crate::error::Error;
use crate::spec::Spec;

/// A method, upload, or event that the running VCam does not have but a newer API does
pub fn missing_name(running: &Spec, known: &Spec, name: &str) -> Option<Error> {
    let since = if let Some(method) = known.method(name) {
        method.since()
    } else if known.upload_prefixes().contains(&name) {
        known.method(&format!("{name}.begin"))?.since()
    } else {
        known.events().find(|e| e.name() == name)?.since()
    };
    Some(unavailable(running, name, since))
}

/// A `--flag` of `method` that the running VCam does not have but a newer API does
pub fn missing_param(running: &Spec, known: &Spec, method: &str, args: &[String]) -> Option<Error> {
    let running_method = running.method(method)?;
    let known_method = known.method(method)?;
    args.iter()
        .filter_map(|arg| arg.strip_prefix("--"))
        .map(|flag| flag.split_once('=').map_or(flag, |(name, _)| name))
        .filter(|name| running_method.param(name).is_none())
        .find_map(|name| {
            let param = known_method.param(name)?;
            Some(unavailable(
                running,
                &format!("--{name} of {method}"),
                param.since,
            ))
        })
}

fn unavailable(running: &Spec, what: &str, since: Option<&str>) -> Error {
    let current = running.version();
    Error::Usage(match since {
        Some(since) => format!(
            "{what} needs VCam API {since} or later, but this VCam has VCam API {current}. Update the VCam app to use it."
        ),
        None => format!("{what} is not available in this VCam (VCam API {current})."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// The bundled spec with `scene.rename` and `--easing` of `scene.load` marked as added in 0.2.0
    fn newer() -> Spec {
        let mut document: Value =
            serde_json::from_str(include_str!("../spec/openrpc.json")).unwrap();
        document["info"]["version"] = json!("0.2.0");
        for method in document["methods"].as_array_mut().unwrap() {
            match method["name"].as_str().unwrap() {
                "scene.rename" => method["x-vcam-since"] = json!("0.2.0"),
                "scene.load" => {
                    for param in method["params"].as_array_mut().unwrap() {
                        if param["name"] == "easing" {
                            param["x-vcam-since"] = json!("0.2.0");
                        }
                    }
                }
                _ => {}
            }
        }
        Spec::new(document)
    }

    /// What a VCam on 0.1.0 would serve for `newer`
    fn older() -> Spec {
        let mut document: Value =
            serde_json::from_str(include_str!("../spec/openrpc.json")).unwrap();
        document["info"]["version"] = json!("0.1.0");
        let methods = document["methods"].as_array_mut().unwrap();
        methods.retain(|m| m["name"] != "scene.rename" && m["name"] != "motion.started");
        for method in methods.iter_mut().filter(|m| m["name"] == "scene.load") {
            method["params"]
                .as_array_mut()
                .unwrap()
                .retain(|p| p["name"] != "easing");
        }
        Spec::new(document)
    }

    #[test]
    fn names_the_version_a_method_needs() {
        let error = missing_name(&older(), &newer(), "scene.rename").unwrap();
        assert_eq!(
            error.to_string(),
            "scene.rename needs VCam API 0.2.0 or later, but this VCam has VCam API 0.1.0. Update the VCam app to use it."
        );
    }

    #[test]
    fn names_the_version_a_parameter_needs() {
        let args: Vec<String> = ["--sceneId", "1", "--easing=linear"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let error = missing_param(&older(), &newer(), "scene.load", &args).unwrap();
        assert!(
            error
                .to_string()
                .starts_with("--easing of scene.load needs VCam API 0.2.0")
        );
        assert!(missing_param(&newer(), &newer(), "scene.load", &args).is_none());
    }

    #[test]
    fn says_what_is_missing_even_without_a_version() {
        let error = missing_name(&older(), &newer(), "motion.started").unwrap();
        assert_eq!(
            error.to_string(),
            "motion.started is not available in this VCam (VCam API 0.1.0)."
        );
    }

    #[test]
    fn leaves_unknown_names_to_the_typo_hints() {
        assert!(missing_name(&older(), &newer(), "scene.lod").is_none());
        let args = vec!["--nope".to_string()];
        assert!(missing_param(&older(), &newer(), "scene.load", &args).is_none());
    }
}
