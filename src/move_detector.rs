//! Move detection for file renames.
//!
//! This module detects when a file has been renamed rather than deleted and recreated.
//! It uses the Sørensen–Dice coefficient to compare content similarity between
//! deleted files (in snapshot but not on disk) and new files (on disk but not in
//! snapshot/remote).

use std::collections::HashSet;

/// Compute the Sørensen–Dice coefficient between two byte slices.
///
/// Returns a value between 0.0 (completely different) and 1.0 (identical).
/// Uses bigrams (2-byte sequences) for comparison.
///
/// The formula is: 2 * |intersection| / (|bigrams_a| + |bigrams_b|)
pub fn sorensen_dice_similarity(a: &[u8], b: &[u8]) -> f64 {
    // Handle edge cases
    if a.is_empty() && b.is_empty() {
        return 1.0; // Both empty = identical
    }
    if a.is_empty() || b.is_empty() {
        return 0.0; // One empty, one not = completely different
    }
    if a.len() == 1 && b.len() == 1 {
        // Single byte: direct comparison
        return if a[0] == b[0] { 1.0 } else { 0.0 };
    }
    if a.len() == 1 || b.len() == 1 {
        // One single byte, other has bigrams: no overlap possible
        return 0.0;
    }

    // Build bigram sets
    let bigrams_a: HashSet<[u8; 2]> = a.windows(2).map(|w| [w[0], w[1]]).collect();
    let bigrams_b: HashSet<[u8; 2]> = b.windows(2).map(|w| [w[0], w[1]]).collect();

    // Count intersection
    let intersection_count = bigrams_a.intersection(&bigrams_b).count();

    // Sørensen–Dice coefficient
    let total = bigrams_a.len() + bigrams_b.len();
    if total == 0 {
        return 1.0; // Both have no bigrams (shouldn't happen given above checks)
    }

    (2.0 * intersection_count as f64) / total as f64
}

/// Default similarity threshold for move detection (70%)
pub const DEFAULT_MOVE_THRESHOLD: f64 = 0.7;

/// A detected move operation
#[derive(Debug, Clone)]
pub struct DetectedMove {
    /// Name of the deleted file (old name)
    pub old_name: String,
    /// Name of the new file (new name)
    pub new_name: String,
    /// Similarity score between the two files
    pub similarity: f64,
}

/// Candidate for move detection: a file that was deleted
#[derive(Debug, Clone)]
pub struct DeletedFileCandidate {
    pub name: String,
    pub content: Vec<u8>,
}

/// Candidate for move detection: a new local file
#[derive(Debug, Clone)]
pub struct NewFileCandidate {
    pub name: String,
    pub content: Vec<u8>,
}

/// Detect moves between deleted files and new files.
///
/// Returns a list of detected moves. Each file can only participate in one move
/// (the highest similarity match wins if there are multiple candidates).
pub fn detect_moves(
    deleted: &[DeletedFileCandidate],
    new_files: &[NewFileCandidate],
    threshold: f64,
) -> Vec<DetectedMove> {
    if deleted.is_empty() || new_files.is_empty() {
        return Vec::new();
    }

    // Compute all pairwise similarities
    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();

    for (d_idx, deleted_file) in deleted.iter().enumerate() {
        for (n_idx, new_file) in new_files.iter().enumerate() {
            let similarity = sorensen_dice_similarity(&deleted_file.content, &new_file.content);
            if similarity >= threshold {
                candidates.push((d_idx, n_idx, similarity));
            }
        }
    }

    // Sort by similarity descending (highest first)
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    // Greedily assign moves (each file can only be in one move)
    let mut used_deleted: HashSet<usize> = HashSet::new();
    let mut used_new: HashSet<usize> = HashSet::new();
    let mut moves = Vec::new();

    for (d_idx, n_idx, similarity) in candidates {
        if used_deleted.contains(&d_idx) || used_new.contains(&n_idx) {
            continue;
        }

        used_deleted.insert(d_idx);
        used_new.insert(n_idx);

        moves.push(DetectedMove {
            old_name: deleted[d_idx].name.clone(),
            new_name: new_files[n_idx].name.clone(),
            similarity,
        });
    }

    moves
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_content() {
        let a = b"Hello, World!";
        let b = b"Hello, World!";
        assert!((sorensen_dice_similarity(a, b) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_completely_different() {
        let a = b"aaaaaaaaaa";
        let b = b"bbbbbbbbbb";
        assert!((sorensen_dice_similarity(a, b) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_empty_both() {
        assert!((sorensen_dice_similarity(b"", b"") - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_empty_one() {
        assert!((sorensen_dice_similarity(b"hello", b"") - 0.0).abs() < 0.001);
        assert!((sorensen_dice_similarity(b"", b"hello") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_single_byte_same() {
        assert!((sorensen_dice_similarity(b"a", b"a") - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_single_byte_different() {
        assert!((sorensen_dice_similarity(b"a", b"b") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_partial_overlap() {
        // "night" and "nacht" share "n" and some structure
        let a = b"night";
        let b = b"nacht";
        let sim = sorensen_dice_similarity(a, b);
        // Should be between 0 and 1, with some overlap
        assert!(sim > 0.0 && sim < 1.0);
    }

    #[test]
    fn test_similar_content() {
        // Same file with minor edit
        let a = b"function hello() { return 'Hello, World!'; }";
        let b = b"function hello() { return 'Hello, World!!'; }";
        let sim = sorensen_dice_similarity(a, b);
        // Should be very high (small change)
        assert!(sim > 0.9);
    }

    #[test]
    fn test_detect_moves_single_match() {
        let deleted = vec![DeletedFileCandidate {
            name: "old.txt".into(),
            content: b"Hello, World!".to_vec(),
        }];
        let new_files = vec![NewFileCandidate {
            name: "new.txt".into(),
            content: b"Hello, World!".to_vec(),
        }];

        let moves = detect_moves(&deleted, &new_files, 0.7);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].old_name, "old.txt");
        assert_eq!(moves[0].new_name, "new.txt");
        assert!((moves[0].similarity - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_detect_moves_below_threshold() {
        let deleted = vec![DeletedFileCandidate {
            name: "old.txt".into(),
            content: b"aaaaaaaaaa".to_vec(),
        }];
        let new_files = vec![NewFileCandidate {
            name: "new.txt".into(),
            content: b"bbbbbbbbbb".to_vec(),
        }];

        let moves = detect_moves(&deleted, &new_files, 0.7);
        assert!(moves.is_empty());
    }

    #[test]
    fn test_detect_moves_multiple_candidates() {
        // Two deleted files, two new files
        // deleted[0] matches new[1] better
        // deleted[1] matches new[0] better
        let deleted = vec![
            DeletedFileCandidate {
                name: "a.txt".into(),
                content: b"content A original".to_vec(),
            },
            DeletedFileCandidate {
                name: "b.txt".into(),
                content: b"content B original".to_vec(),
            },
        ];
        let new_files = vec![
            NewFileCandidate {
                name: "renamed_b.txt".into(),
                content: b"content B original".to_vec(), // Exact match with deleted[1]
            },
            NewFileCandidate {
                name: "renamed_a.txt".into(),
                content: b"content A original".to_vec(), // Exact match with deleted[0]
            },
        ];

        let moves = detect_moves(&deleted, &new_files, 0.7);
        assert_eq!(moves.len(), 2);

        // Should match correctly (order depends on greedy algorithm)
        let move_names: HashSet<(&str, &str)> = moves
            .iter()
            .map(|m| (m.old_name.as_str(), m.new_name.as_str()))
            .collect();
        assert!(move_names.contains(&("a.txt", "renamed_a.txt")));
        assert!(move_names.contains(&("b.txt", "renamed_b.txt")));
    }

    #[test]
    fn test_detect_moves_one_to_many() {
        // One deleted file, multiple similar new files - should only match one
        let deleted = vec![DeletedFileCandidate {
            name: "original.txt".into(),
            content: b"Hello, World!".to_vec(),
        }];
        let new_files = vec![
            NewFileCandidate {
                name: "copy1.txt".into(),
                content: b"Hello, World!".to_vec(),
            },
            NewFileCandidate {
                name: "copy2.txt".into(),
                content: b"Hello, World!".to_vec(),
            },
        ];

        let moves = detect_moves(&deleted, &new_files, 0.7);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].old_name, "original.txt");
        // Should match one of the copies (whichever comes first with equal similarity)
    }

    #[test]
    fn test_detect_moves_empty_inputs() {
        let deleted: Vec<DeletedFileCandidate> = vec![];
        let new_files: Vec<NewFileCandidate> = vec![];
        assert!(detect_moves(&deleted, &new_files, 0.7).is_empty());

        let deleted = vec![DeletedFileCandidate {
            name: "old.txt".into(),
            content: b"content".to_vec(),
        }];
        assert!(detect_moves(&deleted, &[], 0.7).is_empty());
        assert!(detect_moves(&[], &new_files, 0.7).is_empty());
    }
}
