/// Merges overlapping and adjacent closed ranges, so a client can binary
/// search or scan a short list instead of unioning hunks itself.
pub fn merge(mut ranges: Vec<[u32; 2]>) -> Vec<[u32; 2]> {
    ranges.sort_unstable();
    let mut merged: Vec<[u32; 2]> = Vec::with_capacity(ranges.len());
    for [start, end] in ranges {
        match merged.last_mut() {
            Some(last) if start <= last[1].saturating_add(1) => last[1] = last[1].max(end),
            _ => merged.push([start, end]),
        }
    }
    merged
}

pub fn contains(ranges: &[[u32; 2]], line: u32) -> bool {
    ranges.iter().any(|[start, end]| (*start..=*end).contains(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_and_adjacent_ranges_merge() {
        assert_eq!(merge(vec![[8, 10], [1, 6], [7, 9], [20, 22]]), vec![[1, 10], [20, 22]]);
    }

    #[test]
    fn contains_is_inclusive() {
        let r = merge(vec![[3, 5]]);
        assert!(contains(&r, 3) && contains(&r, 5));
        assert!(!contains(&r, 2) && !contains(&r, 6));
    }
}
