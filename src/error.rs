use std::fmt;

use serde_json::{Value, json};

#[derive(Debug)]
pub enum Error {
    /// VCam answered with a JSON-RPC error
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// The command line was wrong, so nothing was sent
    Usage(String),
    Connect {
        url: String,
        reason: String,
    },
    /// The handshake was refused, which happens when the token is missing or wrong
    Rejected {
        url: String,
        status: u16,
    },
    Closed,
    TimedOut,
    Interrupted,
    Io(String),
}

impl Error {
    pub fn from_rpc(error: &Value) -> Self {
        Self::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error")
                .to_owned(),
            data: error.get("data").cloned(),
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Rpc { .. } | Self::Io(_) | Self::Interrupted => 1,
            Self::Usage(_) => 2,
            Self::Connect { .. } | Self::Rejected { .. } | Self::Closed => 3,
            Self::TimedOut => 4,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Rpc {
                code,
                message,
                data,
            } => {
                let mut error = json!({ "code": code, "message": message });
                if let Some(data) = data {
                    error["data"] = data.clone();
                }
                error
            }
            _ => json!({ "message": self.to_string() }),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rpc { message, data, .. } => {
                match data
                    .as_ref()
                    .and_then(|d| d.get("code"))
                    .and_then(Value::as_str)
                {
                    Some(code) => write!(f, "{message} ({code})"),
                    None => write!(f, "{message}"),
                }
            }
            Self::Usage(message) | Self::Io(message) => write!(f, "{message}"),
            Self::Connect { url, reason } => write!(
                f,
                "Could not connect to VCam at {url} ({reason}). Start VCam and turn on the VCam API in its settings."
            ),
            Self::Rejected { url, status } => write!(
                f,
                "VCam at {url} refused the connection (HTTP {status}). Connections from other devices need VCAM_TOKEN set to the token shown in the VCam API settings."
            ),
            Self::Closed => write!(f, "The connection to VCam was closed."),
            Self::TimedOut => write!(f, "Timed out."),
            Self::Interrupted => write!(f, "Interrupted."),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}
