/// Anything that can go wrong between a program and the grid it draws on.
///
/// Every variant carries the terminal library's own message, because that is the only
/// account of what happened worth passing on.
#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    /// An error of your own, for a [`Tty`](crate::Tty) implementation to return.
    pub fn msg(message: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_string(),
        }
    }

    pub(crate) fn context(context: &str, cause: impl std::fmt::Display) -> Self {
        Self {
            message: format!("{context}: {cause}"),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// The result of anything that talks to the emulator or to the program behind it.
pub type Result<T> = std::result::Result<T, Error>;
