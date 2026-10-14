/// Fixed microsecond precision: jiff's default drops trailing zeros, which
/// would break lexical ordering of timestamps (`…45Z` sorts after `…45.9Z`).
pub fn now() -> String {
    format!("{:.6}", jiff::Timestamp::now())
}
