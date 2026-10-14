use crate::model::{Hunk, Stats};

/// The hunks of one `diff --git` block. Status, blobs and the authoritative
/// path come from `git diff --raw` instead; `path` here only exists to join
/// the two, so it must survive quoting and `a/` `b/` prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub path: String,
    pub binary: bool,
    pub stats: Stats,
    /// Zero-context hunks, as the contract exposes them.
    pub hunks: Vec<Hunk>,
    /// The `@@` header ranges, i.e. changed lines plus the 3 lines of context
    /// git printed: exactly what GitHub accepts comments on.
    pub context_base: Vec<[u32; 2]>,
    pub context_head: Vec<[u32; 2]>,
    /// Bytes of this block, so callers can decide `too_large` without
    /// re-scanning.
    pub bytes: usize,
}

pub fn parse(text: &str) -> Vec<Patch> {
    let mut patches = Vec::new();
    let mut current: Option<Patch> = None;
    let mut hunk: Option<HunkState> = None;

    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        if let Some(rest) = body.strip_prefix("diff --git ") {
            if let Some(mut patch) = current.take() {
                finish_hunk(&mut patch, hunk.take());
                patches.push(patch);
            }
            current = Some(new_patch(rest));
            continue;
        }
        let Some(patch) = current.as_mut() else { continue };
        patch.bytes += line.len();

        if let Some(header) = body.strip_prefix("@@ ") {
            finish_hunk(patch, hunk.take());
            hunk = HunkState::from_header(header, patch);
            continue;
        }
        if let Some(state) = hunk.as_mut() {
            match body.as_bytes().first() {
                // An empty body is a blank context line (`diff.suppressBlankEmpty`
                // form, forced off but handled anyway).
                Some(b' ') | None => {
                    flush_change(patch, state);
                    state.base_line += 1;
                    state.head_line += 1;
                }
                Some(b'-') => {
                    state.change().base_count += 1;
                    state.base_line += 1;
                    patch.stats.removed += 1;
                }
                Some(b'+') => {
                    state.change().head_count += 1;
                    state.head_line += 1;
                    patch.stats.added += 1;
                }
                // "\ No newline at end of file" carries no line.
                _ => {}
            }
            continue;
        }

        if let Some(rest) = body
            .strip_prefix("rename to ")
            .or_else(|| body.strip_prefix("copy to "))
        {
            patch.path = unquote(rest);
        } else if body.starts_with("Binary files ") || body.starts_with("GIT binary patch") {
            patch.binary = true;
        } else if let Some(rest) = body.strip_prefix("+++ ")
            && rest != "/dev/null"
        {
            patch.path = strip_prefix_quoted(rest, "b/");
        } else if let Some(rest) = body.strip_prefix("--- ")
            && rest != "/dev/null"
            && patch.path.is_empty()
        {
            patch.path = strip_prefix_quoted(rest, "a/");
        }
    }
    if let Some(mut patch) = current.take() {
        finish_hunk(&mut patch, hunk.take());
        patches.push(patch);
    }
    patches
}

/// `diff --git a/X b/Y` has no delimiter that cannot appear in a path. Quoted
/// headers are exact; for unquoted ones the common X == Y case has two halves
/// of equal length, which the `---` / `+++` / `rename to` lines then confirm
/// or override.
fn new_patch(header: &str) -> Patch {
    let path = if header.starts_with('"') {
        split_quoted(header)
            .map(|(_, second)| strip_prefix_quoted(second, "b/"))
            .unwrap_or_default()
    } else {
        header
            .strip_prefix("a/")
            .and_then(|rest| {
                let len = rest.len().checked_sub(" b/".len())?;
                if len % 2 != 0 {
                    return None;
                }
                let (left, right) = rest.split_at(len / 2);
                right
                    .strip_prefix(" b/")
                    .filter(|r| *r == left)
                    .map(|_| left.to_string())
            })
            .unwrap_or_default()
    };
    Patch {
        path,
        binary: false,
        stats: Stats::default(),
        hunks: Vec::new(),
        context_base: Vec::new(),
        context_head: Vec::new(),
        bytes: header.len(),
    }
}

/// `---` / `+++` paths are TAB-terminated when they contain whitespace, and
/// C-quoted when they contain `"`, `\` or control characters.
fn strip_prefix_quoted(field: &str, prefix: &str) -> String {
    let path = unquote(field.trim_end_matches('\t'));
    path.strip_prefix(prefix).map(str::to_string).unwrap_or(path)
}

fn split_quoted(header: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (i, c) in header.char_indices().skip(1) {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some((&header[..=i], header.get(i + 2..)?)),
            _ => escaped = false,
        }
    }
    None
}

/// git's C-style quoting: `\"`, `\\`, `\t`, `\n`, `\r`, and `\ooo` octal
/// bytes (which may form multi-byte UTF-8 sequences).
pub fn unquote(field: &str) -> String {
    let Some(inner) = field.strip_prefix('"').and_then(|f| f.strip_suffix('"')) else {
        return field.to_string();
    };
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let end = (i + 3).min(bytes.len());
                let digits = &inner[i..end];
                let len = digits.bytes().take_while(|b| (b'0'..=b'7').contains(b)).count();
                out.push(u8::from_str_radix(&digits[..len], 8).unwrap_or(b'?'));
                i += len;
                continue;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct HunkState {
    base_line: u32,
    head_line: u32,
    change: Option<Change>,
}

struct Change {
    first_base: u32,
    first_head: u32,
    base_count: u32,
    head_count: u32,
}

impl HunkState {
    /// `@@ -a[,b] +c[,d] @@`. A zero count means the range is empty and `a`
    /// is the line before it, so counting starts at `a + 1`.
    fn from_header(header: &str, patch: &mut Patch) -> Option<Self> {
        let mut parts = header.split(' ');
        let (base_start, base_count) = range(parts.next()?.strip_prefix('-')?)?;
        let (head_start, head_count) = range(parts.next()?.strip_prefix('+')?)?;
        if base_count > 0 {
            patch.context_base.push([base_start, base_start + base_count - 1]);
        }
        if head_count > 0 {
            patch.context_head.push([head_start, head_start + head_count - 1]);
        }
        Some(Self {
            base_line: if base_count == 0 { base_start + 1 } else { base_start },
            head_line: if head_count == 0 { head_start + 1 } else { head_start },
            change: None,
        })
    }

    /// The pending change starts at the current counters; later `-` / `+`
    /// lines only grow the counts.
    fn change(&mut self) -> &mut Change {
        self.change.get_or_insert(Change {
            first_base: self.base_line,
            first_head: self.head_line,
            base_count: 0,
            head_count: 0,
        })
    }
}

fn range(spec: &str) -> Option<(u32, u32)> {
    match spec.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
    }
}

fn flush_change(patch: &mut Patch, state: &mut HunkState) {
    let Some(change) = state.change.take() else { return };
    patch.hunks.push(Hunk {
        base_start: if change.base_count > 0 {
            change.first_base
        } else {
            change.first_base - 1
        },
        base_count: change.base_count,
        head_start: if change.head_count > 0 {
            change.first_head
        } else {
            change.first_head - 1
        },
        head_count: change.head_count,
    });
}

fn finish_hunk(patch: &mut Patch, hunk: Option<HunkState>) {
    if let Some(mut state) = hunk {
        flush_change(patch, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = include_str!("../../tests/fixtures/diff/mixed.diff");

    fn by_path(path: &str) -> Patch {
        parse(MIXED)
            .into_iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no patch for {path}"))
    }

    #[test]
    fn every_block_in_the_fixture_is_keyed_by_its_exact_path() {
        let paths: Vec<String> = parse(MIXED).into_iter().map(|f| f.path).collect();
        assert_eq!(
            paths,
            vec![
                "added.txt",
                "bin.dat",
                "deleted.txt",
                "keep.txt",
                "mode.sh",
                "renamed_to.txt",
                "dir/with space.txt",
                "quo\"te.txt",
                "tab\tname.txt",
                "link",
                "link",
            ]
        );
    }

    #[test]
    fn added_file_is_one_insert_hunk_before_line_zero() {
        let f = by_path("added.txt");
        assert_eq!(
            f.hunks,
            vec![Hunk {
                base_start: 0,
                base_count: 0,
                head_start: 1,
                head_count: 2
            }]
        );
        assert_eq!(f.context_head, vec![[1, 2]]);
        assert!(f.context_base.is_empty());
        assert_eq!(f.stats, Stats { added: 2, removed: 0 });
    }

    #[test]
    fn deleted_file_is_one_delete_hunk_before_head_line_zero() {
        let f = by_path("deleted.txt");
        assert_eq!(
            f.hunks,
            vec![Hunk {
                base_start: 1,
                base_count: 1,
                head_start: 0,
                head_count: 0
            }]
        );
    }

    #[test]
    fn modified_file_yields_replace_and_append_hunks_with_context_ranges() {
        let f = by_path("keep.txt");
        assert_eq!(
            f.hunks,
            vec![
                Hunk {
                    base_start: 3,
                    base_count: 1,
                    head_start: 3,
                    head_count: 1
                },
                Hunk {
                    base_start: 10,
                    base_count: 0,
                    head_start: 11,
                    head_count: 1
                },
            ]
        );
        assert_eq!(f.context_base, vec![[1, 6], [8, 10]]);
        assert_eq!(f.context_head, vec![[1, 6], [8, 11]]);
        assert_eq!(f.stats, Stats { added: 2, removed: 1 });
    }

    #[test]
    fn binary_and_mode_only_blocks_have_no_hunks() {
        assert!(by_path("bin.dat").binary);
        assert!(by_path("bin.dat").hunks.is_empty());
        assert!(by_path("mode.sh").hunks.is_empty());
    }

    #[test]
    fn a_minus_run_followed_by_a_plus_run_is_one_replace_hunk() {
        let f = by_path("dir/with space.txt");
        assert_eq!(
            f.hunks,
            vec![Hunk {
                base_start: 2,
                base_count: 1,
                head_start: 2,
                head_count: 2
            }]
        );
    }

    #[test]
    fn blank_context_line_without_the_leading_space_still_counts() {
        let text = "diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1,3 +1,3 @@\n a\n\n-b\n+B\n";
        let f = &parse(text)[0];
        assert_eq!(
            f.hunks,
            vec![Hunk {
                base_start: 3,
                base_count: 1,
                head_start: 3,
                head_count: 1
            }]
        );
    }

    #[test]
    fn c_quoted_paths_are_unquoted() {
        assert_eq!(unquote(r#""quo\"te.txt""#), "quo\"te.txt");
        assert_eq!(unquote(r#""tab\tname.txt""#), "tab\tname.txt");
        assert_eq!(unquote(r#""back\\slash""#), "back\\slash");
        assert_eq!(unquote(r#""\343\201\202.txt""#), "あ.txt");
        assert_eq!(unquote("plain.txt"), "plain.txt");
    }

    #[test]
    fn empty_input_yields_no_patches() {
        assert!(parse("").is_empty());
    }
}
