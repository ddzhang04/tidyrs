use crate::dedup::DupGroup;
use crate::dirdup::DirDup;
use crate::plan::{Action, Group, GroupFile, GroupKind};

pub fn build(dups: Vec<DupGroup>, dir_dups: Vec<DirDup>) -> Vec<Group> {
    let mut groups: Vec<Group> = dups.into_iter().map(dup_to_group).collect();
    groups.extend(dir_dups.into_iter().map(dirdup_to_group));
    groups.sort_by_key(|g| std::cmp::Reverse(reclaimable_bytes(g)));
    groups
}

fn dirdup_to_group(d: DirDup) -> Group {
    let label = format!("dir:{}", short_hash(&d.hash));
    let suggested = Action::KeepOne {
        keep: d.keep.clone(),
        trash: d.trash.clone(),
    };
    let files = d
        .dirs
        .iter()
        .map(|de| GroupFile {
            path: de.path.clone(),
            size: de.total_size,
        })
        .collect();
    Group {
        kind: GroupKind::DuplicateDir,
        files,
        label,
        suggested,
    }
}

fn dup_to_group(d: DupGroup) -> Group {
    let label = format!("hash:{}", short_hash(&d.hash));
    let suggested = Action::KeepOne {
        keep: d.keep.clone(),
        trash: d.trash.clone(),
    };
    let files = d
        .files
        .iter()
        .map(|f| GroupFile {
            path: f.path.clone(),
            size: f.size,
        })
        .collect();
    Group {
        kind: GroupKind::Duplicate,
        files,
        label,
        suggested,
    }
}

fn short_hash(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(12);
    for byte in &hash[..6] {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

pub fn reclaimable_bytes(g: &Group) -> u64 {
    match &g.suggested {
        Action::KeepOne { trash, .. } => {
            let unit = g.files.first().map(|f| f.size).unwrap_or(0);
            unit * trash.len() as u64
        }
        Action::FoldIntoFolder { .. } | Action::Ignore => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walker::FileEntry;
    use std::path::PathBuf;

    fn dup(hash_byte: u8, size: u64, paths: Vec<&str>) -> DupGroup {
        let files: Vec<FileEntry> = paths
            .iter()
            .map(|p| FileEntry {
                path: PathBuf::from(p),
                size,
                mtime: 0,
            })
            .collect();
        let keep = files[0].path.clone();
        let trash = files[1..].iter().map(|f| f.path.clone()).collect();
        DupGroup {
            hash: [hash_byte; 32],
            files,
            keep,
            trash,
        }
    }

    #[test]
    fn dup_group_becomes_duplicate_kind() {
        let groups = build(vec![dup(0xab, 100, vec!["/a/x.txt", "/a/y.txt"])], vec![]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, GroupKind::Duplicate);
        assert!(groups[0].label.starts_with("hash:"));
        assert_eq!(groups[0].files.len(), 2);
    }

    #[test]
    fn suggested_action_is_keep_one() {
        let groups = build(vec![dup(0x01, 100, vec!["/a/x.txt", "/a/y.txt"])], vec![]);
        match &groups[0].suggested {
            Action::KeepOne { keep, trash } => {
                assert_eq!(keep, &PathBuf::from("/a/x.txt"));
                assert_eq!(trash.len(), 1);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn sorted_by_reclaimable_desc() {
        let small = dup(0x01, 10, vec!["/a/s1", "/a/s2"]); // reclaim 10
        let big = dup(0x02, 1000, vec!["/a/b1", "/a/b2"]); // reclaim 1000
        let groups = build(vec![small, big], vec![]);
        assert_eq!(groups[0].label, format!("hash:{}", "02".repeat(6)));
        assert!(reclaimable_bytes(&groups[0]) > reclaimable_bytes(&groups[1]));
    }

    #[test]
    fn reclaimable_counts_trash_only() {
        let groups = build(vec![dup(0x01, 100, vec!["/a", "/b", "/c"])], vec![]);
        // 3 files, keep 1, trash 2 → reclaim = 200
        assert_eq!(reclaimable_bytes(&groups[0]), 200);
    }
}
