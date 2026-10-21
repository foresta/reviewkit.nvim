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

    /// The history is saved but sent empty: it only grows, and every
    /// response carries the session. `history list` returns it.
    pub fn with_session(data: impl Serialize, session: &Session) -> Result<Self> {
        let mut response = Self::new(data)?;
        let mut value =
            serde_json::to_value(session).map_err(|err| Error::internal(format!("cannot serialize session: {err}")))?;
        value["history"] = serde_json::json!([]);
        response.session = Some(value);
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
    ExitCode::from(emit_to(&mut std::io::stdout().lock(), result))
}

fn emit_to(out: &mut impl Write, result: Result<Response>) -> u8 {
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
    match writeln!(out, "{json}").and_then(|()| out.flush()) {
        Ok(()) => code,
        Err(err) => {
            // The reader went away before reading the answer; there is no one
            // to hand an envelope to, so only the exit code can say it failed.
            if err.kind() == std::io::ErrorKind::BrokenPipe {
                log::debug!("stdout was closed before the response was written");
            } else {
                eprintln!("reviewkit: cannot write the response to stdout: {err}");
            }
            crate::error::ErrorCode::EIo.exit_code()
        }
    }
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

    struct Failing(std::io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(self.0.into())
        }
    }

    #[test]
    fn a_response_that_cannot_be_written_exits_with_the_io_code() {
        for kind in [std::io::ErrorKind::BrokenPipe, std::io::ErrorKind::StorageFull] {
            let code = emit_to(&mut Failing(kind), Response::new(serde_json::json!({})));
            assert_eq!(code, crate::error::ErrorCode::EIo.exit_code(), "{kind:?}");
        }
        assert_eq!(emit_to(&mut Vec::new(), Response::new(serde_json::json!({}))), 0);
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
