use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::model::{SCHEMA_REVISION, SCHEMA_VERSION, VersionInfo};

pub fn run(_ctx: &Context) -> Result<Response> {
    Response::new(VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: SCHEMA_VERSION,
        schema_revision: SCHEMA_REVISION,
    })
}
