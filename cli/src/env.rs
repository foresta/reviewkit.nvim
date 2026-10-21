/// Empty counts as unset so that `VAR= reviewkit ...` behaves like an unset
/// VAR for every variable, instead of each call site deciding differently.
pub fn env_non_empty(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}
