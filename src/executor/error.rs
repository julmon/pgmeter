//! One error type for everything the executor does, so a failure anywhere in
//! the pipeline reaches `main` as a value with a complete, printable message
//! instead of being printed and `process::exit`ed on the spot.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Spec file, YAML, validation, or a script file the spec references.
    Spec(String),
    /// Lua load or runtime failure (transaction, load or binding script).
    Lua(String),
    /// Database error. Displayed with the server message flattened from the
    /// source chain (`postgres::Error`'s own Display is only "db error").
    Postgres(postgres::Error),
    /// Filesystem failure, with what was being done to which path.
    Io {
        context: String,
        source: std::io::Error,
    },
    /// Anything else: aggregation, report, bookkeeping.
    Other(String),
}

impl Error {
    pub fn spec(msg: impl Into<String>) -> Self {
        Error::Spec(msg.into())
    }

    pub fn lua(msg: impl Into<String>) -> Self {
        Error::Lua(msg.into())
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Spec(m) | Error::Lua(m) | Error::Other(m) => f.write_str(m),
            Error::Postgres(e) => f.write_str(&error_detail(e)),
            Error::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<postgres::Error> for Error {
    fn from(e: postgres::Error) -> Self {
        Error::Postgres(e)
    }
}

impl From<mlua::Error> for Error {
    fn from(e: mlua::Error) -> Self {
        Error::Lua(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::io("I/O error", e)
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(e: serde_yaml::Error) -> Self {
        Error::Spec(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Other(e.to_string())
    }
}

/// Plain-text failures built with `format!` in the loaders and the Lua bridge.
impl From<String> for Error {
    fn from(msg: String) -> Self {
        Error::Other(msg)
    }
}

impl From<&str> for Error {
    fn from(msg: &str) -> Self {
        Error::Other(msg.to_string())
    }
}

/// Flatten an error's source chain into one line. `postgres::Error`'s Display
/// prints only a terse category such as "db error"; the actual server message
/// (e.g. `ERROR: relation "warehouse" already exists`) lives in its sources.
pub fn error_detail(error: &dyn std::error::Error) -> String {
    let mut msg = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !msg.contains(&text) {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        source = cause.source();
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_carries_the_full_message() {
        let e = Error::io(
            "Failed to read spec file 'x.yaml'",
            std::io::Error::new(std::io::ErrorKind::NotFound, "No such file"),
        );
        assert_eq!(
            e.to_string(),
            "Failed to read spec file 'x.yaml': No such file"
        );
        assert_eq!(Error::spec("bad spec").to_string(), "bad spec");
        let lua: Error = mlua::Error::RuntimeError("boom".into()).into();
        assert!(
            matches!(&lua, Error::Lua(m) if m.contains("boom")),
            "{lua:?}"
        );
        let s: Error = String::from("plain").into();
        assert!(matches!(s, Error::Other(_)));
    }

    #[test]
    fn error_detail_flattens_sources_without_repeating() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl fmt::Display for Outer {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("outer failed")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Outer(std::io::Error::other("disk gone"));
        assert_eq!(error_detail(&e), "outer failed: disk gone");
        // A source whose text is already part of the message is not repeated.
        let plain = std::io::Error::other("just this");
        assert_eq!(error_detail(&plain), "just this");
    }
}
