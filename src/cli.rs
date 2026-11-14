use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::args::{parse_params, suggestions};
use crate::client::{Connection, Endpoint};
use crate::compat;
use crate::error::Error;
use crate::spec::{Method, Spec, type_label};
use crate::upload::upload;

#[derive(Clone, Copy)]
pub struct Output {
    pub json: bool,
}

impl Output {
    pub fn print(&self, value: &Value) {
        if self.json || !std::io::stdout().is_terminal() {
            println!("{value}");
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_default()
            );
        }
    }

    pub fn print_error(&self, error: &Error) {
        if self.json {
            eprintln!("{}", error.to_json());
        } else {
            eprintln!("error: {error}");
        }
    }
}

/// Connects and reads the spec of the running VCam, so the commands follow its version
pub async fn session(endpoint: &Endpoint) -> Result<(Connection, Spec), Error> {
    let connection = Connection::connect(endpoint).await?;
    let document = connection.call("rpc.discover", Map::new()).await?;
    Ok((connection, Spec::new(document)))
}

/// Help works without VCam, from the copy of the spec taken at build time
async fn spec_for_help(endpoint: &Endpoint) -> Spec {
    match session(endpoint).await {
        Ok((_, spec)) => spec,
        Err(_) => {
            eprintln!(
                "note: VCam is not reachable, so this describes the API version {}.",
                Spec::bundled().version()
            );
            Spec::bundled()
        }
    }
}

pub async fn call(endpoint: &Endpoint, args: Vec<String>, mut output: Output) -> Result<(), Error> {
    let (name, rest) = args
        .split_first()
        .ok_or_else(|| Error::Usage("No method given".into()))?;
    let mut rest: Vec<String> = rest.to_vec();
    if let Some(index) = rest.iter().position(|a| a == "--json") {
        rest.remove(index);
        output.json = true;
    }
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        return describe(endpoint, name, output).await;
    }

    let (connection, spec) = session(endpoint).await?;
    let known = Spec::bundled();
    if let Some(method) = spec.method(name) {
        if let Some(error) = compat::missing_param(&spec, &known, name, &rest) {
            return Err(error);
        }
        let capture_path = if name == "frame.capture" {
            take_output_path(&mut rest)?
        } else {
            None
        };
        let mut params = parse_params(&spec, method, &rest, &[])?;
        if let Some(path) = &capture_path
            && !params.contains_key("format")
        {
            params.insert("format".into(), json!(capture_format(path)));
        }
        let result = connection.call(name, params).await?;
        match capture_path {
            Some(path) => output.print(&save_capture(result, &path)?),
            None => output.print(&result),
        }
        return Ok(());
    }
    if spec.upload_prefixes().contains(&name.as_str()) {
        let commit = spec
            .method(&format!("{name}.commit"))
            .expect("upload prefixes have a commit method");
        let usage = || {
            Error::Usage(format!(
                "Usage: vcam {name} <file> {}",
                flag_summary(&spec, commit, &["importId"])
            ))
        };
        let (file, flags) = rest.split_first().ok_or_else(usage)?;
        if file.starts_with("--") {
            return Err(usage());
        }
        if let Some(error) = compat::missing_param(&spec, &known, commit.name(), flags) {
            return Err(error);
        }
        let params = parse_params(&spec, commit, flags, &["importId"])?;
        let interrupt = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        let result = upload(&connection, name, Path::new(file), params, interrupt).await?;
        output.print(&result);
        return Ok(());
    }
    Err(compat::missing_name(&spec, &known, name).unwrap_or_else(|| unknown_method(&spec, name)))
}

fn unknown_method(spec: &Spec, name: &str) -> Error {
    let close = suggestions(spec, name);
    if close.is_empty() {
        Error::Usage(format!(
            "Unknown method '{name}'. Run `vcam methods` for the list."
        ))
    } else {
        Error::Usage(format!(
            "Unknown method '{name}'. Did you mean {}?",
            close.join(", ")
        ))
    }
}

fn take_output_path(args: &mut Vec<String>) -> Result<Option<PathBuf>, Error> {
    let Some(index) = args.iter().position(|a| a == "-o" || a == "--output") else {
        return Ok(None);
    };
    if index + 1 >= args.len() {
        return Err(Error::Usage("-o needs a file path".into()));
    }
    let path = args.remove(index + 1);
    args.remove(index);
    Ok(Some(PathBuf::from(path)))
}

fn capture_format(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "png",
        _ => "jpeg",
    }
}

fn save_capture(mut result: Value, path: &Path) -> Result<Value, Error> {
    let Some(Value::String(data)) = result.as_object_mut().and_then(|r| r.remove("data")) else {
        return Err(Error::Io("frame.capture returned no image".into()));
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| Error::Io(format!("frame.capture returned an invalid image: {e}")))?;
    std::fs::write(path, bytes)
        .map_err(|e| Error::Io(format!("Could not write '{}': {e}", path.display())))?;
    result["path"] = json!(path.display().to_string());
    Ok(result)
}

pub async fn methods(endpoint: &Endpoint, output: Output) -> Result<(), Error> {
    let spec = spec_for_help(endpoint).await;
    let mut entries: Vec<(String, String)> = spec
        .methods()
        .filter(|m| m.name() != "rpc.discover")
        .map(|m| {
            (
                m.name().to_owned(),
                m.summary().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    for prefix in spec.upload_prefixes() {
        let summary = spec
            .method(&format!("{prefix}.commit"))
            .and_then(|m| m.summary())
            .unwrap_or_default();
        entries.push((prefix.to_owned(), format!("Uploads a file: {summary}")));
    }
    entries.sort();
    if output.json {
        let list: Vec<Value> = entries
            .iter()
            .map(|(n, s)| json!({ "name": n, "summary": s }))
            .collect();
        output.print(&Value::Array(list));
    } else {
        let width = entries.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        for (name, summary) in entries {
            println!("{name:width$}  {summary}");
        }
    }
    Ok(())
}

pub async fn describe(endpoint: &Endpoint, name: &str, output: Output) -> Result<(), Error> {
    let spec = spec_for_help(endpoint).await;
    if let Some(method) = spec.method(name) {
        print_method(&spec, method, &[], output);
        if name == "frame.capture" {
            println!(
                "  -o, --output <file>  Saves the image to the file. The extension picks the format."
            );
        }
        return Ok(());
    }
    if spec.upload_prefixes().contains(&name) {
        let commit = spec
            .method(&format!("{name}.commit"))
            .expect("upload prefixes have a commit method");
        println!(
            "vcam {name} <file> {}",
            flag_summary(&spec, commit, &["importId"])
        );
        println!();
        println!("Uploads the file with {name}.begin and binary frames, then runs {name}.commit.");
        println!();
        print_method(&spec, commit, &["importId"], output);
        return Ok(());
    }
    Err(compat::missing_name(&spec, &Spec::bundled(), name)
        .unwrap_or_else(|| unknown_method(&spec, name)))
}

fn print_method(spec: &Spec, method: Method<'_>, skip: &[&str], output: Output) {
    if output.json {
        output.print(&json!({
            "name": method.name(),
            "summary": method.summary(),
            "description": method.description(),
            "since": method.since(),
            "params": method.params().iter().filter(|p| !skip.contains(&p.name)).map(|p| json!({
                "name": p.name,
                "required": p.required,
                "schema": p.schema,
                "description": p.description,
                "since": p.since,
            })).collect::<Vec<_>>(),
            "result": method.result_schema(),
        }));
        return;
    }
    println!(
        "{}  {}",
        method.name(),
        method.summary().unwrap_or_default()
    );
    if let Some(since) = method.since() {
        println!("Needs VCam API {since} or later.");
    }
    if let Some(description) = method.description() {
        println!();
        println!("{description}");
    }
    let params: Vec<_> = method
        .params()
        .into_iter()
        .filter(|p| !skip.contains(&p.name))
        .collect();
    if !params.is_empty() {
        println!();
        println!("Parameters:");
        for param in params {
            let required = if param.required { " (required)" } else { "" };
            let since = param
                .since
                .map(|since| format!(" (VCam API {since}+)"))
                .unwrap_or_default();
            println!(
                "  --{} <{}>{required}{since}",
                param.name,
                type_label(spec, param.schema)
            );
            if let Some(description) = param.description {
                println!("      {description}");
            }
        }
    }
    if let Some(result) = method.result_schema() {
        println!();
        println!("Result: {}", type_label(spec, result));
    }
}

fn flag_summary(spec: &Spec, method: Method<'_>, skip: &[&str]) -> String {
    method
        .params()
        .iter()
        .filter(|p| !skip.contains(&p.name))
        .map(|p| {
            let flag = format!("--{} <{}>", p.name, type_label(spec, p.schema));
            if p.required {
                flag
            } else {
                format!("[{flag}]")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn events(
    endpoint: &Endpoint,
    names: Vec<String>,
    count: Option<usize>,
    timeout: Option<f64>,
) -> Result<(), Error> {
    let deadline = match timeout {
        Some(seconds) if seconds.is_finite() && seconds >= 0.0 => {
            Some(Instant::now() + Duration::from_secs_f64(seconds))
        }
        Some(_) => return Err(Error::Usage("--timeout needs a number of seconds".into())),
        None => None,
    };
    let (connection, spec) = session(endpoint).await?;
    let known: Vec<&str> = spec.events().map(|e| e.name()).collect();
    if let Some(unknown) = names.iter().find(|n| !known.contains(&n.as_str())) {
        if let Some(error) = compat::missing_name(&spec, &Spec::bundled(), unknown) {
            return Err(error);
        }
        return Err(Error::Usage(format!(
            "Unknown event '{unknown}'. Events: {}",
            known.join(", ")
        )));
    }
    let mut receiver = connection
        .take_events()
        .expect("a new connection has its events");
    let mut params = Map::new();
    if !names.is_empty() {
        params.insert("events".into(), json!(names));
    }
    connection.call("events.subscribe", params).await?;

    let mut received = 0;
    while count.is_none_or(|c| received < c) {
        let wait_until = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let event = tokio::select! {
            event = receiver.recv() => event.ok_or(Error::Closed)?,
            () = wait_until => return if count.is_some() { Err(Error::TimedOut) } else { Ok(()) },
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        println!("{}", event.to_json());
        received += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_capture_format_from_the_extension() {
        assert_eq!(capture_format(Path::new("a.PNG")), "png");
        assert_eq!(capture_format(Path::new("a.jpg")), "jpeg");
        assert_eq!(capture_format(Path::new("a")), "jpeg");
    }

    #[test]
    fn takes_the_output_path_out_of_the_flags() {
        let mut args: Vec<String> = ["--maxWidth", "640", "-o", "out.png"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            take_output_path(&mut args).unwrap(),
            Some(PathBuf::from("out.png"))
        );
        assert_eq!(args, ["--maxWidth", "640"]);
    }

    #[test]
    fn saves_the_capture_without_the_base64_in_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");
        let result =
            json!({ "data": "aGVsbG8=", "mimeType": "image/png", "width": 2, "height": 1 });
        let printed = save_capture(result, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(printed.get("data").is_none());
        assert_eq!(printed["width"], json!(2));
    }
}
