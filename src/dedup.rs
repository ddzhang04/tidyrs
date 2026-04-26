use crate::cache::{Cache, Key};
use crate::walker::FileEntry;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DupGroup {
    pub hash: [u8; 32],
    pub files: Vec<FileEntry>,
    pub keep: PathBuf,
    pub trash: Vec<PathBuf>,
}

impl DupGroup {
    pub fn reclaimable_bytes(&self) -> u64 {
        let n = self.trash.len() as u64;
        self.files.first().map(|f| f.size).unwrap_or(0) * n
    }
}

pub fn find_duplicates(entries: &[FileEntry], cache: Option<&Cache>) -> Result<Vec<DupGroup>> {
    let mut by_size: HashMap<u64, Vec<&FileEntry>> = HashMap::new();
    for e in entries {
        by_size.entry(e.size).or_default().push(e);
    }

    let mut by_hash: HashMap<[u8; 32], Vec<FileEntry>> = HashMap::new();

    for (_, bucket) in by_size {
        if bucket.len() < 2 {
            continue;
        }
        for entry in bucket {
            let hash = match hash_with_cache(entry, cache) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("hash error for {}: {e}", entry.path.display());
                    continue;
                }
            };
            by_hash.entry(hash).or_default().push(entry.clone());
        }
    }

    let mut groups: Vec<DupGroup> = by_hash
        .into_iter()
        .filter(|(_, files)| files.len() >= 2)
        .map(|(hash, files)| {
            let (keep, trash) = pick_keeper(&files);
            DupGroup { hash, files, keep, trash }
        })
        .collect();

    groups.sort_by(|a, b| b.reclaimable_bytes().cmp(&a.reclaimable_bytes()));
    Ok(groups)
}

fn hash_with_cache(entry: &FileEntry, cache: Option<&Cache>) -> Result<[u8; 32]> {
    let key = Key {
        path: &entry.path,
        size: entry.size,
        mtime: entry.mtime,
    };
    if let Some(c) = cache {
        if let Some(h) = c.get(key)? {
            return Ok(h);
        }
    }
    let h = hash_file(&entry.path)?;
    if let Some(c) = cache {
        c.put(key, h)?;
    }
    Ok(h)
}

fn hash_file(path: &std::path::Path) -> Result<[u8; 32]> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024, f);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn pick_keeper(files: &[FileEntry]) -> (PathBuf, Vec<PathBuf>) {
    let keep_idx = files
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            depth(&a.path)
                .cmp(&depth(&b.path))
                .then_with(|| a.mtime.cmp(&b.mtime))
                .then_with(|| a.path.cmp(&b.path))
        })
        .map(|(i, _)| i)
        .unwrap_or(0);

    let keep = files[keep_idx].path.clone();
    let trash = files
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != keep_idx)
        .map(|(_, f)| f.path.clone())
        .collect();
    (keep, trash)
}

fn depth(p: &std::path::Path) -> usize {
    p.components().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn entry(path: PathBuf, size: u64, mtime: i64) -> FileEntry {
        FileEntry { path, size, mtime }
    }

    fn write(d: &TempDir, rel: &str, bytes: &[u8]) -> FileEntry {
        let p = d.path().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, bytes).unwrap();
        let meta = fs::metadata(&p).unwrap();
        entry(p.canonicalize().unwrap(), meta.len(), 0)
    }

    #[test]
    fn singleton_size_bucket_skipped() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "a.txt", b"unique content");
        let b = write(&d, "b.txt", b"different size!");
        let groups = find_duplicates(&[a, b], None).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn finds_exact_duplicates() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "a.txt", b"hello world");
        let b = write(&d, "sub/b.txt", b"hello world");
        let groups = find_duplicates(&[a, b], None).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[0].trash.len(), 1);
    }

    #[test]
    fn same_size_different_content_no_dup() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "a.txt", b"AAAAA");
        let b = write(&d, "b.txt", b"BBBBB");
        let groups = find_duplicates(&[a, b], None).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn keeper_is_shallowest_path() {
        let d = TempDir::new().unwrap();
        let shallow = write(&d, "top.txt", b"same data here");
        let deep = write(&d, "a/b/c/deep.txt", b"same data here");
        let groups = find_duplicates(&[deep.clone(), shallow.clone()], None).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keep, shallow.path);
        assert_eq!(groups[0].trash, vec![deep.path]);
    }

    #[test]
    fn keeper_tiebreak_oldest_mtime() {
        let d = TempDir::new().unwrap();
        let mut a = write(&d, "a.txt", b"identical bytes");
        let mut b = write(&d, "b.txt", b"identical bytes");
        a.mtime = 100;
        b.mtime = 50;
        let groups = find_duplicates(&[a.clone(), b.clone()], None).unwrap();
        assert_eq!(groups[0].keep, b.path);
    }

    #[test]
    fn three_way_duplicate() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "a.txt", b"triplet content");
        let b = write(&d, "b.txt", b"triplet content");
        let c = write(&d, "c.txt", b"triplet content");
        let groups = find_duplicates(&[a, b, c], None).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 3);
        assert_eq!(groups[0].trash.len(), 2);
    }

    #[test]
    fn cache_round_trips() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "a.txt", b"cached content");
        let b = write(&d, "b.txt", b"cached content");
        let cache = Cache::open_at(&d.path().join("cache.redb")).unwrap();

        let g1 = find_duplicates(&[a.clone(), b.clone()], Some(&cache)).unwrap();
        assert_eq!(g1.len(), 1);

        let key = Key { path: &a.path, size: a.size, mtime: a.mtime };
        assert!(cache.get(key).unwrap().is_some());

        let g2 = find_duplicates(&[a, b], Some(&cache)).unwrap();
        assert_eq!(g2.len(), 1);
        assert_eq!(g1[0].hash, g2[0].hash);
    }

    #[test]
    fn sorted_by_reclaimable_size_desc() {
        let d = TempDir::new().unwrap();
        let small1 = write(&d, "s1.txt", b"smol");
        let small2 = write(&d, "s2.txt", b"smol");
        let big1 = write(&d, "b1.txt", &vec![7u8; 10_000]);
        let big2 = write(&d, "b2.txt", &vec![7u8; 10_000]);
        let groups = find_duplicates(&[small1, small2, big1, big2], None).unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups[0].reclaimable_bytes() > groups[1].reclaimable_bytes());
    }
}
