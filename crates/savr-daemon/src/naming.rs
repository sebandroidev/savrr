//! Title normalization shared by folder→manifest matching (`scan`) and
//! name-based cross-device identity (`engine::game_id_for`).

/// Lowercase and strip every non-ASCII-alphanumeric character, so folder names,
/// manifest `installDir` keys, and titles compare regardless of spacing, case,
/// and punctuation.
pub fn normalize_title(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]

    fn collapses_case_space_and_punctuation() {
        assert_eq!(normalize_title("Hollow Knight"), "hollowknight");
        assert_eq!(normalize_title("HollowKnight"), "hollowknight");
        assert_eq!(normalize_title("  hollow-knight! "), "hollowknight");
        assert_eq!(normalize_title("Baldur's Gate 3"), "baldursgate3");
    }

    #[test]
    fn empty_when_no_alphanumerics() {
        assert_eq!(normalize_title("!!!"), "");
    }
}
