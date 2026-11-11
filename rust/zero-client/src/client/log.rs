//! The client's log (`@rocicorp/logger`).

use std::rc::Rc;

/// How much the client logs (`LogLevel`). A level includes the levels after
/// it: `Warn` logs warnings and errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// Where the client's log messages go (`LogSink`).
pub trait LogSink {
    /// `context` says which client logged the message, such as `clientID`.
    fn log(&self, level: LogLevel, message: &str, context: &[(String, String)]);
}

/// The default sink: standard error.
pub struct StderrLogSink;

impl LogSink for StderrLogSink {
    fn log(&self, level: LogLevel, message: &str, context: &[(String, String)]) {
        let prefix: Vec<String> = context.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if prefix.is_empty() {
            eprintln!("{} {message}", level.as_str());
        } else {
            eprintln!("{} {} {message}", level.as_str(), prefix.join(" "));
        }
    }
}

/// A sink with a level and a context (`LogContext`): the messages below the
/// level are dropped without being built.
#[derive(Clone)]
pub(crate) struct Log {
    pub(crate) level: LogLevel,
    sink: Rc<dyn LogSink>,
    context: Vec<(String, String)>,
}

impl Log {
    pub(crate) fn new(level: LogLevel, sink: Rc<dyn LogSink>) -> Self {
        Log {
            level,
            sink,
            context: Vec::new(),
        }
    }

    pub(crate) fn with(&self, key: &str, value: &str) -> Log {
        let mut copy = self.clone();
        copy.context.retain(|(k, _)| k != key);
        copy.context.push((key.to_owned(), value.to_owned()));
        copy
    }

    pub(crate) fn enabled(&self, level: LogLevel) -> bool {
        level >= self.level
    }

    pub(crate) fn log(&self, level: LogLevel, message: impl FnOnce() -> String) {
        if self.enabled(level) {
            self.sink.log(level, &message(), &self.context);
        }
    }

    pub(crate) fn debug(&self, message: impl FnOnce() -> String) {
        self.log(LogLevel::Debug, message)
    }

    pub(crate) fn info(&self, message: impl FnOnce() -> String) {
        self.log(LogLevel::Info, message)
    }

    pub(crate) fn warn(&self, message: impl FnOnce() -> String) {
        self.log(LogLevel::Warn, message)
    }

    pub(crate) fn error(&self, message: impl FnOnce() -> String) {
        self.log(LogLevel::Error, message)
    }
}
