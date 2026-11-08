use std::future::Future;
use std::path::Path;

use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;

use crate::client::Connection;
use crate::error::Error;

/// VCam reads the importId from the first bytes of every binary frame.
const IMPORT_ID_LENGTH: usize = 36;
/// VCam rejects WebSocket messages over 1 MiB, header included.
const MAX_FRAME_LENGTH: usize = 1024 * 1024;
const CHUNK_LENGTH: usize = MAX_FRAME_LENGTH - IMPORT_ID_LENGTH;

/// Runs `<prefix>.begin`, the binary frames, and `<prefix>.commit`, and cancels the upload
/// when sending fails or `interrupt` fires before the commit is answered.
pub async fn upload(
    connection: &Connection,
    prefix: &str,
    path: &Path,
    commit_params: Map<String, Value>,
    interrupt: impl Future<Output = ()>,
) -> Result<Value, Error> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Usage(format!("'{}' is not a file", path.display())))?
        .to_owned();
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| Error::Io(format!("Could not open '{}': {e}", path.display())))?;

    let begin = connection
        .call(
            &format!("{prefix}.begin"),
            object(json!({ "filename": filename })),
        )
        .await?;
    let import_id = begin
        .get("importId")
        .and_then(Value::as_str)
        .filter(|id| id.len() == IMPORT_ID_LENGTH)
        .ok_or_else(|| Error::Io(format!("{prefix}.begin returned no importId")))?
        .to_owned();

    let transfer = async {
        send_file(connection, &import_id, &mut file).await?;
        let mut params = commit_params;
        params.insert("importId".into(), Value::String(import_id.clone()));
        connection.call(&format!("{prefix}.commit"), params).await
    };
    let outcome = tokio::select! {
        biased;
        () = interrupt => Err(Error::Interrupted),
        outcome = transfer => outcome,
    };
    // VCam removes the staging area itself once it has answered a commit
    if outcome
        .as_ref()
        .is_err_and(|e| !matches!(e, Error::Rpc { .. }))
        && !connection.is_closed()
    {
        let _ = connection
            .call(
                &format!("{prefix}.cancel"),
                object(json!({ "importId": import_id })),
            )
            .await;
    }
    outcome
}

async fn send_file(
    connection: &Connection,
    import_id: &str,
    file: &mut tokio::fs::File,
) -> Result<(), Error> {
    let mut chunk = vec![0; CHUNK_LENGTH];
    loop {
        let mut filled = 0;
        while filled < CHUNK_LENGTH {
            let read = file.read(&mut chunk[filled..]).await?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        if filled == 0 {
            return Ok(());
        }
        let mut frame = Vec::with_capacity(IMPORT_ID_LENGTH + filled);
        frame.extend_from_slice(import_id.as_bytes());
        frame.extend_from_slice(&chunk[..filled]);
        connection.send_binary(frame).await?;
        if filled < CHUNK_LENGTH {
            return Ok(());
        }
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Endpoint;
    use crate::testing::FakeServer;
    use std::io::Write;

    const IMPORT_ID: &str = "00000000-0000-0000-0000-000000000001";

    fn endpoint(port: u16) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port,
            token: None,
        }
    }

    #[tokio::test]
    async fn splits_the_file_into_frames_vcam_accepts() {
        let server = FakeServer::start(|method, _| match method {
            "avatar.import.begin" => Ok(json!({ "importId": IMPORT_ID })),
            "avatar.import.commit" => Ok(json!({ "avatarId": "a" })),
            _ => Err(json!({ "code": -32601, "message": "not found" })),
        })
        .await;
        let content: Vec<u8> = (0..CHUNK_LENGTH * 2 + 10)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&content).unwrap();

        let connection = Connection::connect(&endpoint(server.port)).await.unwrap();
        let result = upload(
            &connection,
            "avatar.import",
            file.path(),
            object(json!({ "load": true })),
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({ "avatarId": "a" }));

        let frames = server.binary_frames();
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|f| f.len() <= MAX_FRAME_LENGTH && f.starts_with(IMPORT_ID.as_bytes()))
        );
        let received: Vec<u8> = frames
            .iter()
            .flat_map(|f| f[IMPORT_ID_LENGTH..].to_vec())
            .collect();
        assert_eq!(received, content);
        let commit = server
            .requests()
            .into_iter()
            .find(|(m, _)| m == "avatar.import.commit")
            .unwrap();
        assert_eq!(commit.1, json!({ "importId": IMPORT_ID, "load": true }));
    }

    #[tokio::test]
    async fn cancels_when_interrupted() {
        let server = FakeServer::start(|method, _| match method {
            "image.import.begin" => Ok(json!({ "importId": IMPORT_ID })),
            _ => Ok(json!(true)),
        })
        .await;
        let file = tempfile::NamedTempFile::new().unwrap();
        let connection = Connection::connect(&endpoint(server.port)).await.unwrap();
        let outcome = upload(
            &connection,
            "image.import",
            file.path(),
            Map::new(),
            std::future::ready(()),
        )
        .await;
        assert!(matches!(outcome, Err(Error::Interrupted)));
        let methods: Vec<String> = server.requests().into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, ["image.import.begin", "image.import.cancel"]);
    }

    #[tokio::test]
    async fn leaves_a_rejected_commit_to_vcam() {
        let server = FakeServer::start(|method, _| match method {
            "avatar.import.begin" => Ok(json!({ "importId": IMPORT_ID })),
            _ => Err(json!({ "code": 1006, "message": "Invalid VRM", "data": { "code": "invalid_vrm" } })),
        })
        .await;
        let file = tempfile::NamedTempFile::new().unwrap();
        let connection = Connection::connect(&endpoint(server.port)).await.unwrap();
        let outcome = upload(
            &connection,
            "avatar.import",
            file.path(),
            Map::new(),
            std::future::pending(),
        )
        .await;
        assert_eq!(
            outcome.unwrap_err().to_string(),
            "Invalid VRM (invalid_vrm)"
        );
        let methods: Vec<String> = server.requests().into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, ["avatar.import.begin", "avatar.import.commit"]);
    }
}
