use crate::model::Model;
use crate::tree::{self, ExpandState, Row, Tree};

pub fn is_match(label: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    if query.chars().any(char::is_uppercase) {
        label.contains(query)
    } else {
        label.to_lowercase().contains(&query.to_lowercase())
    }
}

/// Row order with every session and directory open, respecting session and touched-only filters.
pub fn full_rows(model: &Model, tree: &Tree, touched_only: bool) -> Vec<Row> {
    let state = ExpandState {
        all_open: true,
        expanded_sessions: (0..model.sessions.len()).collect(),
        touched_only,
        auto_follow: false,
        ..Default::default()
    };
    tree::build_rows(model, tree, &state)
}

/// Pick the next matching full-row index, wrapping when no match remains in the direction.
pub fn step(matches: &[usize], cur: Option<usize>, forward: bool, n: usize) -> Option<(usize, bool)> {
    if matches.is_empty() {
        return None;
    }
    let mut pos = cur;
    let mut wrapped = false;
    for _ in 0..n.max(1) {
        let next = if forward {
            matches.iter().copied().find(|&i| pos.is_none_or(|p| i > p))
        } else {
            matches.iter().rev().copied().find(|&i| pos.is_none_or(|p| i < p))
        };
        if next.is_none() {
            wrapped = true;
        }
        pos = next.or_else(|| (if forward { matches.first() } else { matches.last() }).copied());
    }
    pos.map(|idx| (idx, wrapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_search_uses_smartcase() {
        assert!(is_match("Main.rs", "main"));
        assert!(is_match("Main.rs", "Main"));
        assert!(!is_match("Main.rs", "MAIN"));
        assert!(!is_match("Main.rs", ""));
    }

    #[test]
    fn steps_wrap_in_both_directions() {
        let matches = [2, 5, 9];
        assert_eq!(step(&matches, Some(5), true, 1), Some((9, false)));
        assert_eq!(step(&matches, Some(5), true, 2), Some((2, true)));
        assert_eq!(step(&matches, Some(2), false, 1), Some((9, true)));
        assert_eq!(step(&matches, None, true, 1), Some((2, false)));
        assert_eq!(step(&[5], Some(5), true, 1), Some((5, true)));
        assert_eq!(step(&[], None, true, 1), None);
    }
}
