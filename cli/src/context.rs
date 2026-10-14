use std::path::PathBuf;

use crate::store::StateDir;

/// Everything already resolved: no `Option` for values that have a default,
/// so commands never re-derive defaults. Built by `GlobalArgs::resolve`.
#[derive(Debug, Clone)]
pub struct Context {
    pub state_dir: StateDir,
    pub cwd: PathBuf,
    pub log_level: log::LevelFilter,
    pub log_file: Option<PathBuf>,
    pub tools: Tools,
}

/// Overridable through `REVIEWKIT_GIT` / `REVIEWKIT_GH` / `REVIEWKIT_HERDR`
/// because GUI editors often start with a minimal PATH, and tests need to
/// substitute fakes.
#[derive(Debug, Clone)]
pub struct Tools {
    pub git: Option<PathBuf>,
    pub gh: Option<PathBuf>,
    pub herdr: Option<PathBuf>,
}
