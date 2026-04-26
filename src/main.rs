mod actions;
mod cache;
mod dedup;
mod plan;
mod report;
mod walker;

fn main() -> anyhow::Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let opts = walker::WalkOpts {
        root,
        ..Default::default()
    };

    let entries = walker::walk(&opts)?;
    println!("scanned {} files", entries.len());

    let cache = cache::Cache::open()?;
    let groups = dedup::find_duplicates(&entries, Some(&cache))?;

    let mut total_reclaim: u64 = 0;
    for g in &groups {
        total_reclaim += g.reclaimable_bytes();
        println!(
            "\nduplicate group ({} files, reclaim {} bytes)",
            g.files.len(),
            g.reclaimable_bytes()
        );
        println!("  keep:  {}", g.keep.display());
        for t in &g.trash {
            println!("  trash: {}", t.display());
        }
    }
    println!(
        "\n{} duplicate groups, {} bytes reclaimable",
        groups.len(),
        total_reclaim
    );
    Ok(())
}
