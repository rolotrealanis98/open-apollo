use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// Not an engine tree, snapshot or device map.
    UnrecognizedFormat,
    /// The engine answered `{"error":…}`.
    Engine {
        path: String,
        message: String,
    },
    NotConnected,
    Timeout,
    /// The connection was lost while a request was in flight.
    Disconnected,
    /// `set`/`post` on a read-only connection (`Options::allow_writes` is false).
    WriteRefused,
    /// The handle was closed.
    Closed,
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "JSON: {e}"),
            Error::UnrecognizedFormat => f.write_str("not an engine tree, snapshot or device map"),
            Error::Engine { path, message } => write!(f, "{path}: {message}"),
            Error::NotConnected => f.write_str("not connected"),
            Error::Timeout => f.write_str("request timed out"),
            Error::Disconnected => f.write_str("connection lost"),
            Error::WriteRefused => f.write_str("write refused on a read-only connection"),
            Error::Closed => f.write_str("connection closed"),
            Error::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
