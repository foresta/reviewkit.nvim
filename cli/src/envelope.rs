use std::io::Write;
use std::process::ExitCode;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::model::Session;

#[derive(Debug, Default, Serialize)]
pub struct Response {
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<serde_json::Value>,
}

impl Response {
    pub fn new(data: impl Serialize) -> Result<Self> {
        Ok(Self {
            data: serde_json::to_value(data)
                .map_err(|err| Error::internal(format!("cannot serialize response: {err}")))?,
            session: None,
        })
    }

    pub fn with_session(data: impl Serialize, session: &Session) -> Result<Self> {
        let mut response = Self::new(data)?;
        response.session = Some(
            serde_json::to_value(session).map_err(|err| Error::internal(format!("cannot serialize session: {err}")))?,
        );
        Ok(response)
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Envelope<'a> {
    Ok {
        ok: bool,
        #[serde(flatten)]
        response: &'a Response,
    },
    Err {
        ok: bool,
        error: &'a Error,
    },
}

pub fn emit(result: Result<Response>) -> ExitCode {
    let (body, code) = match &result {
        Ok(response) => (Envelope::Ok { ok: true, response }, 0),
        Err(error) => (Envelope::Err { ok: false, error }, error.code.exit_code()),
    };
    let json = serde_json::to_string(&body).unwrap_or_else(|err| {
        // The envelope itself failed to serialize: still emit valid JSON.
        let fallback = Error::internal(format!("failed to serialize envelope: {err}"));
        serde_json::to_string(&Envelope::Err {
            ok: false,
            error: &fallback,
        })
        .expect("static envelope")
    });
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{json}");
    let _ = stdout.flush();
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_omits_session_when_absent() {
        let response = Response::new(serde_json::json!({"a": 1})).unwrap();
        let json = serde_json::to_value(Envelope::Ok {
            ok: true,
            response: &response,
        })
        .unwrap();
        assert_eq!(json, serde_json::json!({"ok": true, "data": {"a": 1}}));
    }

    #[test]
    fn err_envelope_carries_the_error() {
        let error = Error::usage("bad");
        let json = serde_json::to_value(Envelope::Err {
            ok: false,
            error: &error,
        })
        .unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "E_USAGE");
    }
}
