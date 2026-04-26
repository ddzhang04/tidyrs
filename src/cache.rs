use anyhow::{Context, Result};
use directories::ProjectDirs;
use redb::{Database, ReadableTable, TableDefinition};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const TABLE: TableDefinition<&[u8], [u8; 32]> = TableDefinition::new("hashes");

pub struct Cache {
    db: Database,
}

#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    pub path: &'a Path,
    pub size: u64,
    pub mtime: i64,
}

impl Cache {
    pub fn open() -> Result<Self> {
        let path = default_cache_path()?;
        Self::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create cache dir {}", parent.display()))?;
        }
        let db = Database::create(path)
            .with_context(|| format!("open cache {}", path.display()))?;
        let wtx = db.begin_write()?;
        {
            let _ = wtx.open_table(TABLE)?;
        }
        wtx.commit()?;
        Ok(Self { db })
    }

    pub fn rebuild() -> Result<Self> {
        let path = default_cache_path()?;
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove cache {}", path.display()))?;
        }
        Self::open_at(&path)
    }

    pub fn get(&self, key: Key) -> Result<Option<[u8; 32]>> {
        let rtx = self.db.begin_read()?;
        let table = rtx.open_table(TABLE)?;
        let k = encode_key(key);
        Ok(table.get(k.as_slice())?.map(|v| v.value()))
    }

    pub fn put(&self, key: Key, hash: [u8; 32]) -> Result<()> {
        let wtx = self.db.begin_write()?;
        {
            let mut table = wtx.open_table(TABLE)?;
            table.insert(encode_key(key).as_slice(), hash)?;
        }
        wtx.commit()?;
        Ok(())
    }

    pub fn sweep(&self) -> Result<usize> {
        let mut to_delete: Vec<Vec<u8>> = Vec::new();
        {
            let rtx = self.db.begin_read()?;
            let table = rtx.open_table(TABLE)?;
            for row in table.iter()? {
                let (k, _) = row?;
                let bytes = k.value().to_vec();
                let path = decode_key_path(&bytes);
                if !path.exists() {
                    to_delete.push(bytes);
                }
            }
        }
        if to_delete.is_empty() {
            return Ok(0);
        }
        let wtx = self.db.begin_write()?;
        {
            let mut table = wtx.open_table(TABLE)?;
            for k in &to_delete {
                table.remove(k.as_slice())?;
            }
        }
        wtx.commit()?;
        Ok(to_delete.len())
    }
}

fn default_cache_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "tidy")
        .context("could not resolve XDG cache dir")?;
    Ok(dirs.cache_dir().join("hashes.redb"))
}

fn encode_key(k: Key) -> Vec<u8> {
    let path_bytes = k.path.as_os_str().as_bytes();
    let mut buf = Vec::with_capacity(2 + path_bytes.len() + 8 + 8);
    buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(path_bytes);
    buf.extend_from_slice(&k.size.to_le_bytes());
    buf.extend_from_slice(&k.mtime.to_le_bytes());
    buf
}

fn decode_key_path(bytes: &[u8]) -> PathBuf {
    let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    let path_bytes = &bytes[2..2 + len];
    PathBuf::from(OsStr::from_bytes(path_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (TempDir, Cache) {
        let d = TempDir::new().unwrap();
        let cache = Cache::open_at(&d.path().join("hashes.redb")).unwrap();
        (d, cache)
    }

    #[test]
    fn miss_then_hit() {
        let (_d, c) = fresh();
        let p = PathBuf::from("/tmp/example.txt");
        let k = Key { path: &p, size: 100, mtime: 42 };
        assert!(c.get(k).unwrap().is_none());
        let h = [7u8; 32];
        c.put(k, h).unwrap();
        assert_eq!(c.get(k).unwrap(), Some(h));
    }

    #[test]
    fn size_change_invalidates() {
        let (_d, c) = fresh();
        let p = PathBuf::from("/tmp/example.txt");
        c.put(Key { path: &p, size: 100, mtime: 42 }, [1u8; 32]).unwrap();
        let miss = c.get(Key { path: &p, size: 200, mtime: 42 }).unwrap();
        assert!(miss.is_none());
    }

    #[test]
    fn mtime_change_invalidates() {
        let (_d, c) = fresh();
        let p = PathBuf::from("/tmp/example.txt");
        c.put(Key { path: &p, size: 100, mtime: 42 }, [1u8; 32]).unwrap();
        let miss = c.get(Key { path: &p, size: 100, mtime: 99 }).unwrap();
        assert!(miss.is_none());
    }

    #[test]
    fn put_overwrites() {
        let (_d, c) = fresh();
        let p = PathBuf::from("/tmp/example.txt");
        let k = Key { path: &p, size: 100, mtime: 42 };
        c.put(k, [1u8; 32]).unwrap();
        c.put(k, [2u8; 32]).unwrap();
        assert_eq!(c.get(k).unwrap(), Some([2u8; 32]));
    }

    #[test]
    fn sweep_removes_missing_paths() {
        let d = TempDir::new().unwrap();
        let cache = Cache::open_at(&d.path().join("hashes.redb")).unwrap();

        let real = d.path().join("real.txt");
        std::fs::write(&real, b"hi").unwrap();
        let gone = d.path().join("gone.txt");

        cache.put(Key { path: &real, size: 2, mtime: 0 }, [1u8; 32]).unwrap();
        cache.put(Key { path: &gone, size: 2, mtime: 0 }, [2u8; 32]).unwrap();

        let removed = cache.sweep().unwrap();
        assert_eq!(removed, 1);
        assert!(cache.get(Key { path: &real, size: 2, mtime: 0 }).unwrap().is_some());
        assert!(cache.get(Key { path: &gone, size: 2, mtime: 0 }).unwrap().is_none());
    }

    #[test]
    fn persists_across_reopens() {
        let d = TempDir::new().unwrap();
        let path = d.path().join("hashes.redb");
        let p = PathBuf::from("/tmp/example.txt");
        let k = Key { path: &p, size: 100, mtime: 42 };
        {
            let c = Cache::open_at(&path).unwrap();
            c.put(k, [9u8; 32]).unwrap();
        }
        let c = Cache::open_at(&path).unwrap();
        assert_eq!(c.get(k).unwrap(), Some([9u8; 32]));
    }
}
