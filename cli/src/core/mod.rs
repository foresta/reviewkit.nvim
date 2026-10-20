pub mod anchor;
pub mod apply;
pub mod commentable;
pub mod comments;
pub mod etags;
pub mod files;
pub mod refresh;
pub mod threads;
pub mod time;
pub mod viewed;

/// Above this, a file's hunks are dropped and `too_large` is set, and
/// `file base` returns no lines: a generated or vendored file would otherwise
/// bloat every envelope. One constant so the two never disagree.
pub const TOO_LARGE_BYTES: usize = 1024 * 1024;
