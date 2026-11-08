use std::io::Read;

use serde_json::{Map, Number, Value};

use crate::error::Error;
use crate::spec::{Method, Spec, type_label};

/// Builds the params of `method` from `--name value` flags and `--params <json>`.
/// Names in `skip` are filled in by the caller, so they are neither accepted nor required.
pub fn parse_params(
    spec: &Spec,
    method: Method<'_>,
    args: &[String],
    skip: &[&str],
) -> Result<Map<String, Value>, Error> {
    let mut base = Map::new();
    let mut flags = Map::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let Some(flag) = arg.strip_prefix("--") else {
            return Err(Error::Usage(format!(
                "Unexpected argument '{arg}'. Pass parameters as --name value."
            )));
        };
        let (name, inline_value) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if name == "params" {
            let raw = match inline_value {
                Some(value) => value,
                None => {
                    index += 1;
                    args.get(index)
                        .cloned()
                        .ok_or_else(|| Error::Usage("--params needs a JSON object".into()))?
                }
            };
            base = read_params_object(&raw)?;
            index += 1;
            continue;
        }
        let param = method
            .param(name)
            .filter(|p| !skip.contains(&p.name))
            .ok_or_else(|| unknown_param(method, name, skip))?;
        let is_boolean = spec
            .resolve(param.schema)
            .get("type")
            .and_then(Value::as_str)
            == Some("boolean");
        let raw = match inline_value {
            Some(value) => value,
            None => match args.get(index + 1) {
                Some(next) if !(is_boolean && next.starts_with("--")) => {
                    index += 1;
                    next.clone()
                }
                // A bare boolean flag such as `--load` means true
                _ if is_boolean => "true".to_owned(),
                _ => return Err(Error::Usage(format!("--{name} needs a value"))),
            },
        };
        flags.insert(name.to_owned(), coerce(spec, param.schema, name, &raw)?);
        index += 1;
    }

    for name in base.keys() {
        if method.param(name).is_none() || skip.contains(&name.as_str()) {
            return Err(unknown_param(method, name, skip));
        }
    }
    base.extend(flags);
    for param in method.params() {
        if param.required && !skip.contains(&param.name) && !base.contains_key(param.name) {
            return Err(Error::Usage(format!(
                "{} needs --{} <{}>",
                method.name(),
                param.name,
                type_label(spec, param.schema)
            )));
        }
    }
    Ok(base)
}

fn read_params_object(raw: &str) -> Result<Map<String, Value>, Error> {
    let text = if raw == "-" {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input
    } else {
        raw.to_owned()
    };
    match serde_json::from_str(&text) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(Error::Usage("--params needs a JSON object".into())),
    }
}

fn unknown_param(method: Method<'_>, name: &str, skip: &[&str]) -> Error {
    let known: Vec<String> = method
        .params()
        .iter()
        .filter(|p| !skip.contains(&p.name))
        .map(|p| format!("--{}", p.name))
        .collect();
    if known.is_empty() {
        Error::Usage(format!(
            "{} takes no parameters, but got --{name}",
            method.name()
        ))
    } else {
        Error::Usage(format!(
            "{} has no parameter --{name}. Parameters: {}",
            method.name(),
            known.join(", ")
        ))
    }
}

/// Reads a flag value by the type in the spec rather than by its look,
/// so that `--text 123` stays a string.
fn coerce(spec: &Spec, schema: &Value, name: &str, raw: &str) -> Result<Value, Error> {
    let resolved = spec.resolve(schema);
    let invalid = || {
        Error::Usage(format!(
            "--{name} needs {}, but got '{raw}'",
            type_label(spec, schema)
        ))
    };
    let value = match resolved.get("type").and_then(Value::as_str) {
        Some("string") => Value::String(raw.to_owned()),
        Some("integer") => Value::Number(raw.parse::<i64>().map_err(|_| invalid())?.into()),
        Some("number") => {
            let number = raw.parse::<f64>().map_err(|_| invalid())?;
            Value::Number(Number::from_f64(number).ok_or_else(invalid)?)
        }
        Some("boolean") => Value::Bool(raw.parse::<bool>().map_err(|_| invalid())?),
        Some("object") | Some("array") => {
            let value: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
            let matches = match resolved.get("type").and_then(Value::as_str) {
                Some("object") => value.is_object(),
                _ => value.is_array(),
            };
            if !matches {
                return Err(invalid());
            }
            value
        }
        _ => serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned())),
    };
    if let Some(allowed) = resolved.get("enum").and_then(Value::as_array)
        && !allowed.contains(&value)
    {
        return Err(invalid());
    }
    Ok(value)
}

/// Method names close to a mistyped one, for "did you mean" hints
pub fn suggestions<'a>(spec: &'a Spec, name: &str) -> Vec<&'a str> {
    let mut scored: Vec<(usize, &str)> = spec
        .methods()
        .map(|m| (edit_distance(name, m.name()), m.name()))
        .filter(|(distance, _)| *distance <= 3)
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, name)| name).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(method: &str, args: &[&str]) -> Result<Map<String, Value>, Error> {
        let spec = Spec::bundled();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_params(&spec, spec.method(method).unwrap(), &args, &[])
    }

    #[test]
    fn reads_values_by_the_spec_type() {
        let params = parse(
            "scene.load",
            &["--sceneId", "3", "--duration=0.5", "--easing", "easeInOut"],
        )
        .unwrap();
        assert_eq!(
            Value::Object(params),
            json!({ "sceneId": 3, "duration": 0.5, "easing": "easeInOut" })
        );

        let params = parse("subtitle.set", &["--text", "123"]).unwrap();
        assert_eq!(params["text"], json!("123"));
    }

    #[test]
    fn reads_json_for_objects_and_arrays() {
        let params = parse("text.add", &["--text", "hi", "--placement", r#"{"x":0.1}"#]).unwrap();
        assert_eq!(params["placement"], json!({ "x": 0.1 }));
        assert!(matches!(
            parse("text.add", &["--text", "hi", "--placement", "[1]"]),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn takes_a_bare_boolean_flag_as_true() {
        let params = parse("motion.play", &["--loop", "--motionId", "builtin:wave"]).unwrap();
        assert_eq!(params["loop"], json!(true));
        let params = parse("motion.play", &["--motionId", "builtin:wave", "--loop"]).unwrap();
        assert_eq!(params["loop"], json!(true));
    }

    #[test]
    fn lets_flags_override_params() {
        let params = parse(
            "scene.load",
            &[
                "--params",
                r#"{"sceneId":1,"duration":2}"#,
                "--sceneId",
                "4",
            ],
        )
        .unwrap();
        assert_eq!(
            Value::Object(params),
            json!({ "sceneId": 4, "duration": 2 })
        );
    }

    #[test]
    fn rejects_mistakes_before_sending() {
        assert!(matches!(parse("scene.load", &[]), Err(Error::Usage(_))));
        assert!(matches!(
            parse("scene.load", &["--sceneId", "x"]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            parse("scene.load", &["--sceneId", "1", "--nope", "1"]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            parse("scene.load", &["--params", r#"{"nope":1}"#]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            parse("frame.capture", &["--format", "gif"]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(parse("scene.load", &["3"]), Err(Error::Usage(_))));
    }

    #[test]
    fn skips_names_the_caller_fills_in() {
        let spec = Spec::bundled();
        let commit = spec.method("avatar.import.commit").unwrap();
        let params = parse_params(&spec, commit, &["--load".into()], &["importId"]).unwrap();
        assert_eq!(Value::Object(params), json!({ "load": true }));
        assert!(
            parse_params(
                &spec,
                commit,
                &["--importId".into(), "x".into()],
                &["importId"]
            )
            .is_err()
        );
    }

    #[test]
    fn suggests_close_method_names() {
        let spec = Spec::bundled();
        assert_eq!(suggestions(&spec, "scene.lod").first(), Some(&"scene.load"));
    }
}
