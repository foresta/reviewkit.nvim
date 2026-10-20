use serde::Serialize;

use crate::context::Context;
use crate::envelope::Response;
use crate::error::Result;
use crate::model::{Contract, SCHEMA_REVISION, SCHEMA_VERSION};

#[derive(Serialize)]
struct SchemaOutput {
    schema_version: u32,
    schema_revision: u32,
    schema: schemars::Schema,
}

pub fn run(_ctx: &Context) -> Result<Response> {
    Response::new(SchemaOutput {
        schema_version: SCHEMA_VERSION,
        schema_revision: SCHEMA_REVISION,
        schema: schemars::schema_for!(Contract),
    })
}
