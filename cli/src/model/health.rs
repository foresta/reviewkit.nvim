use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthInfo {
    pub tools: ToolsStatus,
    /// `null` rather than `false` when gh is missing, so "not installed" and
    /// "not logged in" stay distinguishable.
    pub gh_auth: Option<bool>,
    pub state_dir: StateDirStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolsStatus {
    pub git: ToolStatus,
    pub gh: ToolStatus,
    pub herdr: ToolStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolStatus {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

/// The directory is created when missing, so `writable` reflects what a
/// session would actually get.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StateDirStatus {
    pub path: PathBuf,
    pub writable: bool,
}
