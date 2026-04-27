mod actions;
mod cache;
mod dedup;
mod plan;
mod report;
mod ui;
mod walker;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "tidy", version, about = "Local-first file organizer")]
struct Cli {
    /// Directory to scan
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Allow the TUI to execute plans (otherwise dry-run only)
    #[arg(long)]
    execute: bool,

    /// Print a plain report instead of opening the TUI
    #[arg(long)]
    plain: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.path.clone();

    if cli.plain {
        return run_plain(&root);
    }

    let cache = Arc::new(cache::Cache::open()?);
    let initial = ui::Settings::default();

    let factory = {
        let root = root.clone();
        let cache = cache.clone();
        move |settings: ui::Settings| {
            let opts = walker::WalkOpts {
                root: root.clone(),
                min_size: settings.min_size,
                include_hidden: settings.include_hidden,
                use_gitignore: settings.use_gitignore,
            };
            let cache_for_scan = if settings.use_cache {
                Some(cache.clone())
            } else {
                None
            };
            spawn_scan(opts, cache_for_scan)
        }
    };

    let outcome = ui::run_with_factory(cli.execute, initial, factory)?;

    match outcome {
        ui::UiOutcome::Quit => println!("quit"),
        ui::UiOutcome::Save(plan) => {
            let path = root.canonicalize()?.join("tidy-plan.json");
            std::fs::write(&path, plan.to_json()?)?;
            println!("plan written to {}", path.display());
        }
        ui::UiOutcome::Execute(plan) => {
            let log = actions::execute(&plan)?;
            let log_path = actions::write_undo_log(&log)?;
            println!(
                "executed {} actions, undo log: {}",
                log.entries.len(),
                log_path.display()
            );
        }
    }
    Ok(())
}

fn spawn_scan(
    opts: walker::WalkOpts,
    cache: Option<Arc<cache::Cache>>,
) -> std::sync::mpsc::Receiver<ui::Progress> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let walk_tx = tx.clone();
        let entries = match walker::walk_with(&opts, |n| {
            let _ = walk_tx.send(ui::Progress::Walked(n));
        }) {
            Ok(e) => e,
            Err(e) => {
                let _ = tx.send(ui::Progress::Error(e.to_string()));
                return;
            }
        };
        let cache_ref = cache.as_deref();
        let hash_tx = tx.clone();
        let mut started = false;
        let dups = match dedup::find_duplicates_with(&entries, cache_ref, |done, total| {
            if !started {
                let _ = hash_tx.send(ui::Progress::HashStart { total });
                started = true;
            }
            let _ = hash_tx.send(ui::Progress::Hashed { done, total });
        }) {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(ui::Progress::Error(e.to_string()));
                return;
            }
        };
        let groups = report::build(dups);
        let _ = tx.send(ui::Progress::Done(groups));
    });
    rx
}

fn run_plain(root: &PathBuf) -> Result<()> {
    let opts = walker::WalkOpts {
        root: root.clone(),
        ..Default::default()
    };
    let entries = walker::walk(&opts)?;
    eprintln!("scanned {} files", entries.len());
    let cache = cache::Cache::open()?;
    let dups = dedup::find_duplicates(&entries, Some(&cache))?;
    let groups = report::build(dups);
    if groups.is_empty() {
        println!("no duplicate groups found");
        return Ok(());
    }
    let mut total: u64 = 0;
    for g in &groups {
        let bytes = report::reclaimable_bytes(g);
        total += bytes;
        println!(
            "\n{} ({} files, {} bytes reclaimable)",
            g.label,
            g.files.len(),
            bytes
        );
        for f in &g.files {
            println!("  {}", f.path.display());
        }
    }
    println!("\n{} groups, {} bytes total reclaimable", groups.len(), total);
    Ok(())
}
