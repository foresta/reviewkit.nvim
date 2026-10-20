use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

/// From the first release on, codes are only ever added, never renamed.
/// They name what went wrong, not which tool: `Error.tool` says that, so a
/// new sink or source adds no codes. The aliases read states saved before
/// the codes were generic. No per-variant docs: they would turn the JSON
/// Schema into a `oneOf` instead of one `enum` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    EUsage,
    EInvalid,
    ENotCommentable,
    ENoSession,
    ENoComment,
    ENoTarget,
    ENoThread,
    EGit,
    #[serde(alias = "E_GH", alias = "E_HERDR")]
    ETool,
    #[serde(alias = "E_GH_MISSING", alias = "E_HERDR_MISSING")]
    EToolMissing,
    #[serde(alias = "E_GH_AUTH")]
    EAuth,
    #[serde(alias = "E_GH_PENDING_REVIEW")]
    EPendingReview,
    EAmbiguous,
    EConflict,
    ELocked,
    ESchemaVersion,
    EInternal,
}

impl ErrorCode {
    pub fn exit_code(self) -> u8 {
        use ErrorCode::*;
        match self {
            EUsage => 1,
            EInvalid | ENotCommentable => 2,
            ENoSession | ENoComment | ENoTarget | ENoThread => 3,
            EGit | ETool | EToolMissing | EAuth | EPendingReview | EAmbiguous => 4,
            EConflict | ELocked => 5,
            ESchemaVersion => 6,
            EInternal => 70,
        }
    }
}

/// Reuses the serde form so the wire name and the displayed name cannot diverge.
impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(name)) => f.write_str(&name),
            _ => f.write_str("E_INTERNAL"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// The external tool that failed ("gh", "herdr"), for the tool codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    /// Always an object, so callers can index it without a null check.
    #[serde(default = "empty_object")]
    pub details: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            tool: None,
            retryable: false,
            details: empty_object(),
        }
    }

    pub fn tool(tool: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        let mut err = Self::new(code, message);
        err.tool = Some(tool.into());
        err
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn details(mut self, details: impl Serialize) -> Self {
        self.details = match serde_json::to_value(details) {
            Ok(value @ serde_json::Value::Object(_)) => value,
            Ok(value) => serde_json::json!({ "value": value }),
            Err(_) => empty_object(),
        };
        self
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::EUsage, message).hint("run `reviewkit --help`")
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::EInvalid, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::EInternal, message)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::internal(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_uses_the_wire_name() {
        assert_eq!(ErrorCode::EAuth.to_string(), "E_AUTH");
        let old: ErrorCode = serde_json::from_str("\"E_HERDR_MISSING\"").unwrap();
        assert_eq!(old, ErrorCode::EToolMissing);
        assert_eq!(Error::usage("bad").to_string(), "E_USAGE: bad");
    }

    #[test]
    fn exit_codes_follow_the_table() {
        assert_eq!(ErrorCode::EUsage.exit_code(), 1);
        assert_eq!(ErrorCode::EInvalid.exit_code(), 2);
        assert_eq!(ErrorCode::ENoSession.exit_code(), 3);
        assert_eq!(ErrorCode::EAmbiguous.exit_code(), 4);
        assert_eq!(ErrorCode::ELocked.exit_code(), 5);
        assert_eq!(ErrorCode::ESchemaVersion.exit_code(), 6);
        assert_eq!(ErrorCode::EInternal.exit_code(), 70);
    }

    #[test]
    fn error_omits_empty_hint_but_always_has_details_object() {
        let json = serde_json::to_value(Error::internal("boom")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"code": "E_INTERNAL", "message": "boom", "retryable": false, "details": {}})
        );
        let json = serde_json::to_value(Error::internal("boom").details(vec![1])).unwrap();
        assert_eq!(json["details"], serde_json::json!({"value": [1]}));
    }

    #[test]
    fn schema_lists_every_code_as_one_enum() {
        let schema = serde_json::to_value(schemars::schema_for!(ErrorCode)).unwrap();
        let values = schema["enum"].as_array().expect("single enum list, not oneOf");
        assert!(values.contains(&serde_json::json!("E_USAGE")));
        assert!(values.contains(&serde_json::json!("E_INTERNAL")));
    }
}
