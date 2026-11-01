use std::fmt;

/// Errors raised by the protocol state machines.
///
/// Messages never contain secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The peer sent bytes that do not decode as the expected message.
    Malformed(&'static str),
    /// Authentication failed: wrong password, bad MAC or unknown key.
    AuthenticationFailed,
    /// A message arrived in a state where it is not allowed.
    UnexpectedMessage(&'static str),
    /// The peer speaks an unsupported protocol version.
    UnsupportedProtocol(u32),
    /// The server rejected the request with a stable code.
    Rejected { code: String, message: String },
    /// Local misuse or an internal cryptographic failure.
    Internal(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(what) => write!(f, "malformed message: {what}"),
            Error::AuthenticationFailed => f.write_str("authentication failed"),
            Error::UnexpectedMessage(what) => write!(f, "unexpected message: {what}"),
            Error::UnsupportedProtocol(v) => write!(f, "unsupported protocol version {v}"),
            Error::Rejected { code, message } => write!(f, "rejected ({code}): {message}"),
            Error::Internal(what) => write!(f, "internal error: {what}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
