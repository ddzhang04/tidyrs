use crate::cache::Cache;
use crate::dedup::hash_with_cache;
use crate::walker::FileEntry;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub file_count: usize,
    pub total_size: u64,
}

#[derive(Debug, Clone)]
pub struct DirDup {
    pub hash: [u8; 32],
    pub dirs: Vec<DirEntry>,
    pub keep: PathBuf,
    pub trash: Vec<PathBuf>,
}

impl DirDup {
    pub fn reclaimable_bytes(&self) -> u64 {
        let unit = self.dirs.first().map(|d| d.total_size).unwrap_or(0);
        unit * self.trash.len() as u64
    }
}

pub fn find_duplicate_dirs(
    root: &Path,
    entries: &[FileEntry],
    cache: Option<&Cache>,
) -> Result<Vec<DirDup>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    // 1. Cheap pass: count files and sum sizes per ancestor directory.
    let mut dir_count: HashMap<PathBuf, usize> = HashMap::new();
    let mut dir_size: HashMap<PathBuf, u64> = HashMap::new();
    for entry in entries {
        for ancestor in ancestors_up_to(&entry.path, root) {
            *dir_count.entry(ancestor.clone()).or_insert(0) += 1;
            *dir_size.entry(ancestor.clone()).or_insert(0) += entry.size;
        }
    }

    // 2. Bucket dirs by (file_count, total_size). Singleton buckets cannot have a duplicate.
    let mut by_sig: HashMap<(usize, u64), Vec<PathBuf>> = HashMap::new();
    for (dir, count) in &dir_count {
        let size = dir_size[dir];
        // Skip dirs with fewer than 2 files — too noisy as duplicates.
        if *count < 2 {
            continue;
        }
        by_sig.entry((*count, size)).or_default().push(dir.clone());
    }
    let candidates: HashSet<PathBuf> = by_sig
        .values()
        .filter(|v| v.len() >= 2)
        .flat_map(|v| v.iter().cloned())
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // 3. Second pass: gather the file list for each candidate dir (relative path + entry).
    let mut files_per_candidate: HashMap<PathBuf, Vec<(PathBuf, &FileEntry)>> = HashMap::new();
    for entry in entries {
        for ancestor in ancestors_up_to(&entry.path, root) {
            if candidates.contains(&ancestor) {
                if let Ok(rel) = entry.path.strip_prefix(&ancestor) {
                    files_per_candidate
                        .entry(ancestor.clone())
                        .or_default()
                        .push((rel.to_path_buf(), entry));
                }
            }
        }
    }

    // 4. Hash files inside each candidate dir, then compute the dir's content hash.
    let mut by_dir_hash: HashMap<[u8; 32], Vec<DirEntry>> = HashMap::new();
    for (dir, files) in &files_per_candidate {
        let mut hashed: Vec<(PathBuf, [u8; 32])> = Vec::with_capacity(files.len());
        for (rel, entry) in files {
            match hash_with_cache(entry, cache) {
                Ok(h) => hashed.push((rel.clone(), h)),
                Err(e) => {
                    eprintln!("hash error for {}: {e}", entry.path.display());
                    // bail this dir; an unreadable file makes its hash unreliable
                    hashed.clear();
                    break;
                }
            }
        }
        if hashed.len() != files.len() {
            continue;
        }
        hashed.sort_by(|a, b| a.0.cmp(&b.0));

        let mut hasher = blake3::Hasher::new();
        for (rel, h) in &hashed {
            hasher.update(rel.to_string_lossy().as_bytes());
            hasher.update(&[0]);
            hasher.update(h);
        }
        let dir_hash = *hasher.finalize().as_bytes();

        by_dir_hash.entry(dir_hash).or_default().push(DirEntry {
            path: dir.clone(),
            file_count: files.len(),
            total_size: dir_size[dir],
        });
    }

    // 5. Drop ancestor-of-duplicate dirs to avoid double counting.
    //    If A and B are duplicate dirs, every shared sub-tree of A and B is also "duplicate"
    //    by construction. Keep only the topmost duplicate dirs.
    let mut groups: Vec<DirDup> = by_dir_hash
        .into_iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|(hash, dirs)| {
            let (keep, trash) = pick_keeper_dir(&dirs);
            DirDup { hash, dirs, keep, trash }
        })
        .collect();

    groups = drop_nested_duplicates(groups);

    // 6. Sort by reclaimable bytes desc.
    groups.sort_by(|a, b| b.reclaimable_bytes().cmp(&a.reclaimable_bytes()));
    Ok(groups)
}

fn ancestors_up_to(path: &Path, root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = path.parent();
    while let Some(p) = cur {
        if p == root {
            break;
        }
        out.push(p.to_path_buf());
        if p.parent().is_none() {
            break;
        }
        cur = p.parent();
    }
    out
}

fn pick_keeper_dir(dirs: &[DirEntry]) -> (PathBuf, Vec<PathBuf>) {
    let keep_idx = dirs
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            a.path
                .components()
                .count()
                .cmp(&b.path.components().count())
                .then_with(|| a.path.cmp(&b.path))
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let keep = dirs[keep_idx].path.clone();
    let trash = dirs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != keep_idx)
        .map(|(_, d)| d.path.clone())
        .collect();
    (keep, trash)
}

/// Remove duplicate-dir groups whose dirs are all *contained inside* dirs from another group.
/// E.g. if `~/photos/2023/` and `~/backup/2023/` are duplicate, then their child
/// `~/photos/2023/jan/` and `~/backup/2023/jan/` are also duplicate by construction.
/// Keep only the topmost layer.
fn drop_nested_duplicates(groups: Vec<DirDup>) -> Vec<DirDup> {
    let all_dirs: Vec<PathBuf> = groups
        .iter()
        .flat_map(|g| g.dirs.iter().map(|d| d.path.clone()))
        .collect();
    groups
        .into_iter()
        .filter(|g| {
            // group is "nested" iff every dir in the group has a strict ancestor that also
            // appears in some other group's dirs.
            !g.dirs.iter().all(|dir| {
                all_dirs
                    .iter()
                    .any(|other| other != &dir.path && dir.path.starts_with(other))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(d: &Path, rel: &str, bytes: &[u8]) -> FileEntry {
        let p = d.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, bytes).unwrap();
        let meta = fs::metadata(&p).unwrap();
        FileEntry {
            path: p.canonicalize().unwrap(),
            size: meta.len(),
            mtime: 0,
        }
    }

    #[test]
    fn finds_two_identical_subdirs() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();

        let entries = vec![
            write(&root, "a/file1.txt", b"content one"),
            write(&root, "a/file2.txt", b"content two"),
            write(&root, "b/file1.txt", b"content one"),
            write(&root, "b/file2.txt", b"content two"),
        ];

        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.dirs.len(), 2);
        let dir_names: Vec<String> = g
            .dirs
            .iter()
            .map(|d| d.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(dir_names.contains(&"a".to_string()));
        assert!(dir_names.contains(&"b".to_string()));
    }

    #[test]
    fn ignores_subdirs_with_different_content() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();
        let entries = vec![
            write(&root, "a/file1.txt", b"AAAAA"),
            write(&root, "a/file2.txt", b"BBBBB"),
            // b has different content but same file count / total size
            write(&root, "b/file1.txt", b"CCCCC"),
            write(&root, "b/file2.txt", b"DDDDD"),
        ];
        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn ignores_dirs_with_different_filenames() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();
        let entries = vec![
            write(&root, "a/file1.txt", b"hello world"),
            write(&root, "a/file2.txt", b"more content"),
            // same content but renamed → different dir hash
            write(&root, "b/renamed1.txt", b"hello world"),
            write(&root, "b/renamed2.txt", b"more content"),
        ];
        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn does_not_flag_root() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();
        let entries = vec![
            write(&root, "a.txt", b"hello"),
            write(&root, "b.txt", b"world"),
        ];
        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn drops_nested_duplicates() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();
        // two parallel trees with identical structure. The grandparent + child are both dup.
        let entries = vec![
            write(&root, "a/sub/x.txt", b"data"),
            write(&root, "a/sub/y.txt", b"data2"),
            write(&root, "b/sub/x.txt", b"data"),
            write(&root, "b/sub/y.txt", b"data2"),
        ];
        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        // Should only report the top-level a/b duplicates, not a/sub vs b/sub
        assert_eq!(groups.len(), 1);
        let dir_names: Vec<String> = groups[0]
            .dirs
            .iter()
            .map(|d| d.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(dir_names.contains(&"a".to_string()));
        assert!(dir_names.contains(&"b".to_string()));
    }

    #[test]
    fn keeper_is_shallowest() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();
        let entries = vec![
            write(&root, "shallow/a.txt", b"content"),
            write(&root, "shallow/b.txt", b"more content"),
            write(&root, "x/y/z/deep/a.txt", b"content"),
            write(&root, "x/y/z/deep/b.txt", b"more content"),
        ];
        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].keep.ends_with("shallow"));
    }

    #[test]
    fn skips_dirs_with_only_one_file() {
        let d = TempDir::new().unwrap();
        let root = d.path().canonicalize().unwrap();
        let entries = vec![
            write(&root, "a/only.txt", b"single"),
            write(&root, "b/only.txt", b"single"),
        ];
        let groups = find_duplicate_dirs(&root, &entries, None).unwrap();
        // one-file dirs are skipped (would just be regular dup detection)
        assert!(groups.is_empty());
    }
}
