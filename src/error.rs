use std::fmt;

/// molso error carrying the process exit code to use and a message that must
/// never contain credential bytes.
#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: String,
}

impl Error {
    pub fn usage(message: impl Into<String>) -> Self {
        Error {
            code: 2,
            message: message.into(),
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Error {
            code: 2,
            message: message.into(),
        }
    }

    pub fn wrapper(code: i32, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Error::failure(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
