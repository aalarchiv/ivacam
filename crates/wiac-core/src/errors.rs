//! Structured error type — kind + message + recovery hint + optional
//! auto-fix suggestion. Replaces the legacy `error::Error` enum and the
//! ad-hoc `Result<T, String>` returns used by older pipeline code.
//!
//! The struct serializes as flat JSON; the frontend renders it via
//! `ErrorToast.svelte`. `recovery_hint` is an English template; future
//! i18n will resolve placeholders like `{op_name}` against the project.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cam::setup::ToolOffset;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_fix: Option<AutoFix>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<SourceSpan>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    BadInput,
    Misconfigured,
    Limit,
    Unsupported,
    Io,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutoFix {
    AssignTool { op_id: u32, suggested_tool_id: u32 },
    LowerSimResolution { suggested_cell_mm: f64 },
    DisableOp { op_id: u32 },
    ChangeProfileOffset { op_id: u32, suggested: ToolOffset },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            recovery_hint: None,
            auto_fix: None,
            span: None,
        }
    }
    pub fn bad_input(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadInput, msg)
    }
    pub fn misconfigured(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Misconfigured, msg)
    }
    pub fn limit(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Limit, msg)
    }
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, msg)
    }
    pub fn io(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, msg)
    }
    #[must_use] pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.recovery_hint = Some(hint.into());
        self
    }
    #[must_use] pub fn with_auto_fix(mut self, fix: AutoFix) -> Self {
        self.auto_fix = Some(fix);
        self
    }
    #[must_use] pub fn with_span(mut self, span: SourceSpan) -> Self {
        self.span = Some(span);
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.recovery_hint {
            write!(f, " (hint: {hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(e: &Error) -> Error {
        let s = serde_json::to_string(e).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn round_trip_each_kind() {
        for kind in [
            ErrorKind::BadInput,
            ErrorKind::Misconfigured,
            ErrorKind::Limit,
            ErrorKind::Unsupported,
            ErrorKind::Io,
            ErrorKind::Internal,
        ] {
            let e = Error::new(kind, "msg");
            assert_eq!(round_trip(&e), e);
        }
    }

    #[test]
    fn round_trip_each_auto_fix() {
        let cases = [
            AutoFix::AssignTool {
                op_id: 1,
                suggested_tool_id: 7,
            },
            AutoFix::LowerSimResolution {
                suggested_cell_mm: 0.5,
            },
            AutoFix::DisableOp { op_id: 3 },
            AutoFix::ChangeProfileOffset {
                op_id: 4,
                suggested: ToolOffset::Outside,
            },
        ];
        for fix in cases {
            let e = Error::misconfigured("x").with_auto_fix(fix.clone());
            assert_eq!(round_trip(&e), e);
        }
    }

    #[test]
    fn round_trip_with_span_and_hint() {
        let e = Error::bad_input("bad")
            .with_hint("try this")
            .with_span(SourceSpan {
                file: "f.dxf".into(),
                line: 12,
                column: 3,
            });
        assert_eq!(round_trip(&e), e);
    }

    #[test]
    fn display_includes_hint() {
        let e = Error::misconfigured("op 2 references missing tool 9")
            .with_hint("Pick a tool from the library.");
        let s = format!("{e}");
        assert!(s.contains("op 2 references missing tool 9"), "{s}");
        assert!(s.contains("Pick a tool from the library."), "{s}");
    }

    #[test]
    fn display_without_hint_is_just_message() {
        let e = Error::bad_input("oops");
        assert_eq!(format!("{e}"), "oops");
    }
}
