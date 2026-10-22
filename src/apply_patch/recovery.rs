//! Only recover a stale cursor position when the original file uniquely
//! identifies every changed region. Never guess at the model's intentions.

use std::fs;

use codex_apply_patch::{Hunk, UpdateFileChunk};

const MAX_RECOVERY_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn reorder_unique_nonoverlapping_hunks(hunk: &Hunk) -> Option<Hunk> {
    let Hunk::UpdateFile {
        path,
        move_path,
        chunks,
    } = hunk
    else {
        return None;
    };
    if chunks.len() < 2 || fs::metadata(path).ok()?.len() > MAX_RECOVERY_FILE_BYTES {
        return None;
    }

    let contents = fs::read_to_string(path).ok()?;
    // Codex matches logical lines, not CRLF terminators. Recovery must use the
    // same model when the source has Windows or mixed line endings.
    let lines: Vec<&str> = contents
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let mut located = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        // Explicit `@@ symbol` and EOF anchors carry additional model intent.
        // Never bypass those constraints just to make an edit appear to work.
        if chunk.change_context.is_some() || chunk.is_end_of_file {
            return None;
        }
        let (start, len) = unique_start(&lines, chunk)?;
        located.push((start, start + len, index));
    }
    located.sort_unstable();
    if located.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    if located
        .iter()
        .enumerate()
        .all(|(position, &(_, _, index))| position == index)
    {
        return None;
    }

    Some(Hunk::UpdateFile {
        path: path.clone(),
        move_path: move_path.clone(),
        chunks: located
            .iter()
            .map(|(_, _, index)| chunks[*index].clone())
            .collect(),
    })
}

fn unique_start(lines: &[&str], chunk: &UpdateFileChunk) -> Option<(usize, usize)> {
    let old = &chunk.old_lines;
    if old.is_empty() || old.iter().all(String::is_empty) {
        // Pure insertions have no sufficiently strong placement evidence.
        return None;
    }
    locate(lines, old)
        .map(|index| (index, old.len()))
        .or_else(|| {
            if old.len() > 1 && old.last().is_some_and(String::is_empty) {
                locate(lines, &old[..old.len() - 1]).map(|index| (index, old.len() - 1))
            } else {
                None
            }
        })
}

fn locate(lines: &[&str], old: &[String]) -> Option<usize> {
    if old.is_empty() || old.len() > lines.len() {
        return None;
    }
    for matcher in [
        line_equals as fn(&str, &str) -> bool,
        trailing_space_equals,
        surrounding_space_equals,
    ] {
        let mut found = None;
        for start in 0..=lines.len() - old.len() {
            if lines[start..start + old.len()]
                .iter()
                .zip(old)
                .all(|(left, right)| matcher(left, right))
            {
                if found.is_some() {
                    return None; // Ambiguous, never choose one arbitrarily.
                }
                found = Some(start);
            }
        }
        if found.is_some() {
            return found;
        }
    }
    None
}

fn line_equals(left: &str, right: &str) -> bool {
    left == right
}

fn trailing_space_equals(left: &str, right: &str) -> bool {
    left.trim_end() == right.trim_end()
}

fn surrounding_space_equals(left: &str, right: &str) -> bool {
    left.trim() == right.trim()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn reordered_disjoint_unique_chunks_can_be_recovered() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ordered.txt");
        fs::write(&path, "first\nmiddle\nlast\n").unwrap();
        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-last\n+LAST\n@@\n-first\n+FIRST\n*** End Patch",
            path.display()
        );
        let hunks = codex_apply_patch::parse_patch(&patch).unwrap().hunks;
        let reordered = reorder_unique_nonoverlapping_hunks(&hunks[0]).unwrap();
        let Hunk::UpdateFile { chunks, .. } = reordered else {
            panic!("expected updated file");
        };
        assert_eq!(chunks[0].old_lines[0], "first");
        assert_eq!(chunks[1].old_lines[0], "last");
    }

    #[test]
    fn ambiguous_or_overlapping_chunks_are_not_reordered() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ambiguous.txt");
        fs::write(&path, "first\nrepeat\nrepeat\nlast\n").unwrap();
        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-last\n+LAST\n@@\n-repeat\n+REPEAT\n*** End Patch",
            path.display()
        );
        let hunks = codex_apply_patch::parse_patch(&patch).unwrap().hunks;
        assert!(reorder_unique_nonoverlapping_hunks(&hunks[0]).is_none());
    }
}
