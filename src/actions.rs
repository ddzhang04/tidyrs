use crate::plan::{Action, Plan};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UndoEntry {
    Trashed {
        original: PathBuf,
        mtime: i64,
    },
    Moved {
        from: PathBuf,
        to: PathBuf,
        mtime: i64,
    },
    CreatedDir {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoLog {
    pub created_at: i64,
    pub entries: Vec<UndoEntry>,
}

impl UndoLog {
    fn new() -> Self {
        Self {
            created_at: now_secs(),
            entries: Vec::new(),
        }
    }
}

pub fn execute(plan: &Plan) -> Result<UndoLog> {
    let mut log = UndoLog::new();
    if plan.dry_run {
        return Ok(log);
    }
    for action in &plan.actions {
        if let Err(e) = execute_action(action, &mut log) {
            return Err(e.context(format!("aborted at action: {action:?}")));
        }
    }
    Ok(log)
}

fn execute_action(action: &Action, log: &mut UndoLog) -> Result<()> {
    match action {
        Action::Ignore => Ok(()),
        Action::KeepOne { keep: _, trash } => {
            for path in trash {
                let mtime = mtime_of(path).unwrap_or(0);
                trash::delete(path)
                    .with_context(|| format!("trash {}", path.display()))?;
                log.entries.push(UndoEntry::Trashed {
                    original: path.clone(),
                    mtime,
                });
            }
            Ok(())
        }
        Action::FoldIntoFolder { folder_name, files } => {
            let target_dir = pick_folder_dir(files, folder_name)?;
            let created = !target_dir.exists();
            if created {
                std::fs::create_dir_all(&target_dir)
                    .with_context(|| format!("mkdir {}", target_dir.display()))?;
                log.entries.push(UndoEntry::CreatedDir {
                    path: target_dir.clone(),
                });
            }
            for src in files {
                let name = src
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("no file name in {}", src.display()))?;
                let dst = unique_path(&target_dir.join(name));
                let mtime = mtime_of(src).unwrap_or(0);
                std::fs::rename(src, &dst).with_context(|| {
                    format!("move {} -> {}", src.display(), dst.display())
                })?;
                log.entries.push(UndoEntry::Moved {
                    from: src.clone(),
                    to: dst,
                    mtime,
                });
            }
            Ok(())
        }
    }
}

fn pick_folder_dir(files: &[PathBuf], folder_name: &str) -> Result<PathBuf> {
    let parent = files
        .first()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("no parent for fold target"))?;
    Ok(parent.join(folder_name))
}

fn unique_path(desired: &Path) -> PathBuf {
    if !desired.exists() {
        return desired.to_path_buf();
    }
    let stem = desired
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = desired.extension().map(|e| e.to_string_lossy().into_owned());
    let parent = desired.parent().unwrap_or(Path::new("."));
    for n in 1.. {
        let name = match &ext {
            Some(e) => format!("{stem}_{n}.{e}"),
            None => format!("{stem}_{n}"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn mtime_of(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn write_undo_log(log: &UndoLog) -> Result<PathBuf> {
    let dir = undo_log_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let name = format!("undo-{}.json", format_timestamp(log.created_at));
    let path = dir.join(name);
    let json = serde_json::to_string_pretty(log)?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn write_undo_log_to(log: &UndoLog, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(log)?;
    std::fs::write(path, json)?;
    Ok(())
}

fn undo_log_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local/state/tidy"))
}

fn format_timestamp(secs: i64) -> String {
    let days = secs / 86400;
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, m, s)
}

fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let _ = z;
    (y as i32, m as u32, d as u32)
}

#[derive(Debug, Default)]
pub struct UndoReport {
    pub reversed: usize,
    pub skipped_modified: Vec<PathBuf>,
    pub skipped_other: Vec<(PathBuf, String)>,
}

pub fn undo(log: &UndoLog) -> Result<UndoReport> {
    let mut report = UndoReport::default();
    for entry in log.entries.iter().rev() {
        match entry {
            UndoEntry::Moved { from, to, mtime } => match mtime_of(to) {
                Some(current) if current != *mtime => {
                    report.skipped_modified.push(to.clone());
                }
                None => {
                    report
                        .skipped_other
                        .push((to.clone(), "missing".into()));
                }
                Some(_) => {
                    if let Some(parent) = from.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::rename(to, from) {
                        Ok(()) => report.reversed += 1,
                        Err(e) => report.skipped_other.push((to.clone(), e.to_string())),
                    }
                }
            },
            UndoEntry::CreatedDir { path } => {
                if path.exists() {
                    if std::fs::read_dir(path)?.next().is_none() {
                        if std::fs::remove_dir(path).is_ok() {
                            report.reversed += 1;
                        }
                    } else {
                        report
                            .skipped_other
                            .push((path.clone(), "dir not empty".into()));
                    }
                }
            }
            UndoEntry::Trashed { original, .. } => {
                report
                    .skipped_other
                    .push((original.clone(), "trash restore not implemented".into()));
            }
        }
    }
    Ok(report)
}

pub fn load_undo_log(path: &Path) -> Result<UndoLog> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let log: UndoLog = serde_json::from_str(&s)?;
    Ok(log)
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

    #[test]
    fn dry_run_is_noop() {
        let d = TempDir::new().unwrap();
        let f = write(&d, "a.txt", b"hello");
        let plan = Plan {
            dry_run: true,
            actions: vec![Action::KeepOne {
                keep: PathBuf::from("/dev/null"),
                trash: vec![f.clone()],
            }],
        };
        let log = execute(&plan).unwrap();
        assert!(log.entries.is_empty());
        assert!(f.exists());
    }

    #[test]
    fn fold_into_folder_moves_files() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "trip_1.jpg", b"a");
        let b = write(&d, "trip_2.jpg", b"b");
        let c = write(&d, "trip_3.jpg", b"c");
        let plan = Plan {
            dry_run: false,
            actions: vec![Action::FoldIntoFolder {
                folder_name: "trip".into(),
                files: vec![a.clone(), b.clone(), c.clone()],
            }],
        };
        let log = execute(&plan).unwrap();
        let target = d.path().join("trip");
        assert!(target.is_dir());
        assert!(target.join("trip_1.jpg").exists());
        assert!(target.join("trip_2.jpg").exists());
        assert!(target.join("trip_3.jpg").exists());
        assert!(!a.exists());
        // CreatedDir + 3 Moved
        assert_eq!(log.entries.len(), 4);
    }

    #[test]
    fn fold_handles_name_collision() {
        let d = TempDir::new().unwrap();
        fs::create_dir(d.path().join("trip")).unwrap();
        let preexisting = d.path().join("trip/photo.jpg");
        fs::write(&preexisting, b"original").unwrap();
        let incoming = write(&d, "photo.jpg", b"incoming");

        let plan = Plan {
            dry_run: false,
            actions: vec![Action::FoldIntoFolder {
                folder_name: "trip".into(),
                files: vec![incoming.clone()],
            }],
        };
        execute(&plan).unwrap();
        assert!(preexisting.exists());
        assert_eq!(fs::read(&preexisting).unwrap(), b"original");
        assert!(d.path().join("trip/photo_1.jpg").exists());
    }

    #[test]
    fn undo_reverses_moves() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "trip_1.jpg", b"a");
        let b = write(&d, "trip_2.jpg", b"b");
        let plan = Plan {
            dry_run: false,
            actions: vec![Action::FoldIntoFolder {
                folder_name: "trip".into(),
                files: vec![a.clone(), b.clone()],
            }],
        };
        let log = execute(&plan).unwrap();
        assert!(!a.exists());

        let report = undo(&log).unwrap();
        assert!(a.exists());
        assert!(b.exists());
        assert!(!d.path().join("trip").exists());
        assert!(report.reversed >= 2);
    }

    #[test]
    fn undo_skips_modified_file() {
        let d = TempDir::new().unwrap();
        let a = write(&d, "trip_1.jpg", b"original");
        let plan = Plan {
            dry_run: false,
            actions: vec![Action::FoldIntoFolder {
                folder_name: "trip".into(),
                files: vec![a.clone()],
            }],
        };
        let log = execute(&plan).unwrap();
        // mutate after the move so mtime no longer matches
        let moved = d.path().join("trip/trip_1.jpg");
        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&moved, b"changed!").unwrap();

        let report = undo(&log).unwrap();
        assert!(!report.skipped_modified.is_empty());
        assert!(moved.exists());
        assert!(!a.exists());
    }

    #[test]
    fn undo_log_round_trips() {
        let d = TempDir::new().unwrap();
        let log = UndoLog {
            created_at: 1_700_000_000,
            entries: vec![UndoEntry::CreatedDir {
                path: PathBuf::from("/tmp/x"),
            }],
        };
        let path = d.path().join("undo.json");
        write_undo_log_to(&log, &path).unwrap();
        let back = load_undo_log(&path).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.created_at, 1_700_000_000);
    }

    #[test]
    fn timestamp_format_is_yyyymmdd_hhmmss() {
        // 2023-11-14 22:13:20 UTC = 1700000000
        let s = format_timestamp(1_700_000_000);
        assert_eq!(s, "20231114-221320");
    }

}
