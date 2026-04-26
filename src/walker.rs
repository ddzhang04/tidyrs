use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct WalkOpts {
    pub root: PathBuf,
    pub min_size: u64,
    pub include_hidden: bool,
    pub use_gitignore: bool,
}

impl Default for WalkOpts {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            min_size: 1024,
            include_hidden: false,
            use_gitignore: true,
        }
    }
}

pub fn walk(opts: &WalkOpts) -> Result<Vec<FileEntry>> {
    let root = opts
        .root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", opts.root.display()))?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }

    let walker = WalkBuilder::new(&root)
        .hidden(!opts.include_hidden)
        .git_ignore(opts.use_gitignore)
        .git_global(opts.use_gitignore)
        .git_exclude(opts.use_gitignore)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();

    let mut out = Vec::new();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk error: {e}");
                continue;
            }
        };

        match entry.file_type() {
            Some(ft) if ft.is_file() => {}
            _ => continue,
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("stat error for {}: {e}", entry.path().display());
                continue;
            }
        };

        let size = meta.len();
        if size < opts.min_size {
            continue;
        }

        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let path = match entry.path().canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };

        out.push(FileEntry { path, size, mtime });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &TempDir, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.path().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, bytes).unwrap();
        p
    }

    fn opts(root: &TempDir) -> WalkOpts {
        WalkOpts {
            root: root.path().to_path_buf(),
            min_size: 0,
            include_hidden: false,
            use_gitignore: false,
        }
    }

    #[test]
    fn finds_regular_files() {
        let d = TempDir::new().unwrap();
        write(&d, "a.txt", b"hello");
        write(&d, "sub/b.txt", b"world");
        let entries = walk(&opts(&d)).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn skips_under_min_size() {
        let d = TempDir::new().unwrap();
        write(&d, "small.txt", b"x");
        write(&d, "big.txt", &vec![0u8; 2048]);
        let mut o = opts(&d);
        o.min_size = 1024;
        let entries = walk(&o).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.ends_with("big.txt"));
    }

    #[test]
    fn skips_hidden_by_default() {
        let d = TempDir::new().unwrap();
        write(&d, ".secret", b"shhh......");
        write(&d, "visible.txt", b"hello.....");
        let entries = walk(&opts(&d)).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.ends_with("visible.txt"));
    }

    #[test]
    fn includes_hidden_when_flagged() {
        let d = TempDir::new().unwrap();
        write(&d, ".secret", b"shhh......");
        write(&d, "visible.txt", b"hello.....");
        let mut o = opts(&d);
        o.include_hidden = true;
        let entries = walk(&o).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn always_skips_dot_git() {
        let d = TempDir::new().unwrap();
        write(&d, ".git/config", b"[core].........");
        write(&d, "real.txt", b"hello.........");
        let mut o = opts(&d);
        o.include_hidden = true;
        let entries = walk(&o).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.ends_with("real.txt"));
    }

    #[test]
    fn errors_on_non_directory_root() {
        let d = TempDir::new().unwrap();
        let f = write(&d, "file.txt", b"hi");
        let o = WalkOpts {
            root: f,
            ..Default::default()
        };
        assert!(walk(&o).is_err());
    }
}
