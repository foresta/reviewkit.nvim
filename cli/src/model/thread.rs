use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Side;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Thread {
    pub id: String,
    pub path: String,
    pub subject_type: SubjectType,
    pub side: Side,
    pub start_side: Option<Side>,
    /// `null` for file-level threads and for outdated ones, where GitHub no
    /// longer reports a line.
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub original_line: Option<u32>,
    pub resolved: bool,
    pub outdated: bool,
    pub comments: Vec<ThreadComment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SubjectType {
    Line,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThreadComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
}
