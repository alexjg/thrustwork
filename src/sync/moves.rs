//! Move detection for the sync engine.
//!
//! This module detects files that were renamed/moved rather than deleted and recreated.
//! It uses the Sørensen–Dice coefficient to compare content similarity.

use std::collections::HashSet;
use std::path::PathBuf;

use tracing::debug;

use super::sync_ops::get_file_content_at_heads;

use super::classify::{FileChange, SyncPlan};

/// A detected file move/rename.
#[derive(Debug, Clone)]
pub struct DetectedMove {
    /// Path where the file was deleted from
    #[expect(dead_code)]
    pub old_path: PathBuf,
    /// Path where the file appeared
    #[expect(dead_code)]
    pub new_path: PathBuf,
    /// Similarity score
    #[expect(dead_code)]
    pub similarity: f64,
}

/// Detect moves in a sync plan and update it accordingly.
///
/// This looks for patterns where:
/// - A file is marked as LocalDelete (in snapshot+repo but not filesystem)
/// - A file is marked as LocalNew (only in filesystem)
/// - The content is similar enough to be considered a move
///
/// When a move is detected, the LocalDelete and LocalNew entries are replaced
/// with a single Moved entry at the new path.
///
/// Content for deleted files is read from the repo at the heads recorded at sync start,
/// ensuring we compare against the correct baseline even if the repo has changed.
pub fn detect_and_apply_moves(plan: &mut SyncPlan, threshold: f64) -> Vec<DetectedMove> {
    // Collect paths of candidates
    let deleted_paths: Vec<PathBuf> = plan
        .files
        .iter()
        .filter_map(|(path, change)| {
            if matches!(change, FileChange::LocalDelete { .. }) {
                Some(path.clone())
            } else {
                None
            }
        })
        .collect();

    let new_paths: Vec<PathBuf> = plan
        .files
        .iter()
        .filter_map(|(path, change)| {
            if matches!(change, FileChange::LocalNew { .. }) {
                Some(path.clone())
            } else {
                None
            }
        })
        .collect();

    if deleted_paths.is_empty() || new_paths.is_empty() {
        return Vec::new();
    }

    // Build candidates with content
    // For deleted files, read content from repo at the recorded heads
    let mut deleted_with_content: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for path in &deleted_paths {
        if let Some(FileChange::LocalDelete { remote, .. }) = plan.files.get(path) {
            match get_file_content_at_heads(&remote.handle, &remote.heads) {
                Ok(content) => {
                    deleted_with_content.push((path.clone(), content));
                }
                Err(e) => {
                    debug!(path = %path.display(), error = %e, "failed to get remote content for move detection");
                }
            }
        }
    }

    // For new files, use the content from the filesystem state captured at sync start
    let mut new_with_content: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for path in &new_paths {
        if let Some(FileChange::LocalNew { local, .. }) = plan.files.get(path) {
            new_with_content.push((path.clone(), local.content.clone()));
        }
    }

    if deleted_with_content.is_empty() || new_with_content.is_empty() {
        return Vec::new();
    }

    // Compute all pairwise similarities above threshold
    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();
    for (d_idx, (_, deleted_content)) in deleted_with_content.iter().enumerate() {
        for (n_idx, (_, new_content)) in new_with_content.iter().enumerate() {
            let similarity = sorensen_dice_similarity(deleted_content, new_content);
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
    let mut matched_pairs: Vec<(PathBuf, PathBuf, f64)> = Vec::new();

    for (d_idx, n_idx, similarity) in candidates {
        if used_deleted.contains(&d_idx) || used_new.contains(&n_idx) {
            continue;
        }

        used_deleted.insert(d_idx);
        used_new.insert(n_idx);

        let old_path = deleted_with_content[d_idx].0.clone();
        let new_path = new_with_content[n_idx].0.clone();
        matched_pairs.push((old_path, new_path, similarity));
    }

    // Update the plan: remove LocalDelete and LocalNew, add Moved at new_path
    let mut moves = Vec::new();
    for (old_path, new_path, similarity) in matched_pairs {
        // Extract data from LocalDelete
        let (snap, remote) = match plan.files.remove(&old_path) {
            Some(FileChange::LocalDelete { snap, remote }) => (snap, remote),
            _ => continue,
        };

        // Extract data from LocalNew
        let new_content = match plan.files.remove(&new_path) {
            Some(FileChange::LocalNew { local, .. }) => local,
            _ => {
                // Put back the LocalDelete if we can't complete the move
                plan.files
                    .insert(old_path.clone(), FileChange::LocalDelete { snap, remote });
                continue;
            }
        };

        debug!(
            old = %old_path.display(),
            new = %new_path.display(),
            similarity = similarity,
            "detected file move"
        );

        moves.push(DetectedMove {
            old_path: old_path.clone(),
            new_path: new_path.clone(),
            similarity,
        });

        // Insert the Moved entry at the new path
        plan.files.insert(
            new_path,
            FileChange::Moved {
                old_path,
                snap,
                remote,
                new_content,
            },
        );
    }

    moves
}

/// Compute the Sørensen–Dice coefficient between two byte slices.
///
/// Returns a value between 0.0 (completely different) and 1.0 (identical).
/// Uses bigrams (2-byte sequences) for comparison.
///
/// The formula is: 2 * |intersection| / (|bigrams_a| + |bigrams_b|)
fn sorensen_dice_similarity(a: &[u8], b: &[u8]) -> f64 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Default threshold for move detection (70% similarity)
    pub const DEFAULT_MOVE_THRESHOLD: f64 = 0.7;

    #[test]
    fn test_sorensen_identical_content() {
        let a = b"Hello, World!";
        let b = b"Hello, World!";
        assert!((sorensen_dice_similarity(a, b) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_sorensen_completely_different() {
        let a = b"aaaaaaaaaa";
        let b = b"bbbbbbbbbb";
        assert!((sorensen_dice_similarity(a, b) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_sorensen_empty_both() {
        assert!((sorensen_dice_similarity(b"", b"") - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_sorensen_empty_one() {
        assert!((sorensen_dice_similarity(b"hello", b"") - 0.0).abs() < 0.001);
        assert!((sorensen_dice_similarity(b"", b"hello") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_sorensen_single_byte_same() {
        assert!((sorensen_dice_similarity(b"a", b"a") - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_sorensen_single_byte_different() {
        assert!((sorensen_dice_similarity(b"a", b"b") - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_sorensen_partial_overlap() {
        // "night" and "nacht" share some structure
        let a = b"night";
        let b = b"nacht";
        let sim = sorensen_dice_similarity(a, b);
        // Should be between 0 and 1, with some overlap
        assert!(sim > 0.0 && sim < 1.0);
    }

    #[test]
    fn test_sorensen_similar_content() {
        // Same file with minor edit
        let a = b"function hello() { return 'Hello, World!'; }";
        let b = b"function hello() { return 'Hello, World!!'; }";
        let sim = sorensen_dice_similarity(a, b);
        // Should be very high (small change)
        assert!(sim > 0.9);
    }

    #[test]
    fn test_no_moves_when_no_candidates() {
        let mut plan = SyncPlan::new();
        let moves = detect_and_apply_moves(&mut plan, DEFAULT_MOVE_THRESHOLD);
        assert!(moves.is_empty());
    }

    #[test]
    fn test_move_detection_no_deletes() {
        use crate::sync::state::FsFile;
        use samod::AutomergeUrl;

        fn test_url(uuid: &str) -> AutomergeUrl {
            format!("automerge:{}", uuid)
                .parse()
                .expect("invalid test url")
        }

        let mut plan = SyncPlan::new();

        // Add a LocalNew entry
        plan.files.insert(
            PathBuf::from("new_name.txt"),
            FileChange::LocalNew {
                local: FsFile {
                    abs_path: PathBuf::from("/test/new_name.txt"),
                    content: b"Hello, World!".to_vec(),
                    is_text: true,
                    permissions: 0o644,
                    extension: "txt".to_string(),
                    mime_type: "text/plain".to_string(),
                },
                parent_url: Some(test_url("6ba7b810-9dad-11d1-80b4-00c04fd430c8")),
            },
        );

        // Without LocalDelete entries, no moves should be detected
        let moves = detect_and_apply_moves(&mut plan, DEFAULT_MOVE_THRESHOLD);
        assert!(moves.is_empty());

        // The LocalNew should still be there
        assert!(matches!(
            plan.files.get(&PathBuf::from("new_name.txt")),
            Some(FileChange::LocalNew { .. })
        ));
    }
}
