mod cache;
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
    println!("found {} files", entries.len());
    for e in &entries {
        println!("{:>10}  {}", e.size, e.path.display());
    }
    Ok(())
}
