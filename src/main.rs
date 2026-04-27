mod actions;
mod cache;
mod dedup;
mod plan;
mod report;
mod ui;
mod walker;

use anyhow::Result;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let execute_flag = take_flag(&mut args, "--execute");
    let plain = take_flag(&mut args, "--plain");

    let root: PathBuf = args
        .into_iter()
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let opts = walker::WalkOpts {
        root: root.clone(),
        ..Default::default()
    };

    if plain {
        let entries = walker::walk(&opts)?;
        eprintln!("scanned {} files", entries.len());
        let cache = cache::Cache::open()?;
        let dups = dedup::find_duplicates(&entries, Some(&cache))?;
        let groups = report::build(dups);
        if groups.is_empty() {
            println!("no duplicate groups found");
        } else {
            print_plain(&groups);
        }
        return Ok(());
    }

    let cache = cache::Cache::open()?;
    let scan_opts = opts.clone();
    let outcome = ui::run_with_scan(execute_flag, move |tx| {
        let send = |p: ui::Progress| {
            let _ = tx.send(p);
        };
        let walk_tx = tx.clone();
        let entries = match walker::walk_with(&scan_opts, |n| {
            let _ = walk_tx.send(ui::Progress::Walked(n));
        }) {
            Ok(e) => e,
            Err(e) => {
                send(ui::Progress::Error(e.to_string()));
                return;
            }
        };
        let hash_tx = tx.clone();
        let mut started = false;
        let dups = match dedup::find_duplicates_with(&entries, Some(&cache), |done, total| {
            if !started {
                let _ = hash_tx.send(ui::Progress::HashStart { total });
                started = true;
            }
            let _ = hash_tx.send(ui::Progress::Hashed { done, total });
        }) {
            Ok(d) => d,
            Err(e) => {
                send(ui::Progress::Error(e.to_string()));
                return;
            }
        };
        let groups = report::build(dups);
        send(ui::Progress::Done(groups));
    })?;

    match outcome {
        ui::UiOutcome::Quit => {
            println!("quit");
        }
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

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(pos) = args.iter().position(|a| a == flag) {
        args.remove(pos);
        true
    } else {
        false
    }
}

fn print_plain(groups: &[plan::Group]) {
    let mut total: u64 = 0;
    for g in groups {
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
}
