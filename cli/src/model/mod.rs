pub mod health;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Bumped only for incompatible changes. Within one version, fields and enum
/// values are only ever added.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VersionInfo {
    pub version: String,
    pub schema_version: u32,
}

/// Root of the JSON Schema. It only exists so that one `schema_for!` call
/// emits every contract type under `$defs`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Contract {
    pub version: VersionInfo,
    pub health: health::HealthInfo,
    pub error: crate::error::Error,
}
