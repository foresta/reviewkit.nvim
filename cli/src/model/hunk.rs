use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Zero-context hunk. When a count is 0 the start is the line *before* the
/// change, exactly as git prints it (0 means before the first line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hunk {
    pub base_start: u32,
    pub base_count: u32,
    pub head_start: u32,
    pub head_count: u32,
}
