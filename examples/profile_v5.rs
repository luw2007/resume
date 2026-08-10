use std::time::Instant;

fn main() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let codex_home = home.join(".codex");
    let bounds = resume::preview::jsonl::Bounds::default();
    let canonical_root = codex_home.canonicalize().unwrap();

    let roots = resume::integration::codex::rollout_roots(&codex_home);
    let mut timings: Vec<(std::time::Duration, u64, std::path::PathBuf)> = Vec::new();

    for root in &roots {
        for path in walkdir(&root.path) {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let t0 = Instant::now();
            let _ = resume::integration::codex::parse_rollout_file(&path, &canonical_root, &bounds);
            let elapsed = t0.elapsed();
            timings.push((elapsed, size, path));
        }
    }
    timings.sort_by(|a, b| b.0.cmp(&a.0));
    let total: std::time::Duration = timings.iter().map(|t| t.0).sum();
    println!("total: {:?} across {} files", total, timings.len());
    println!("top 15 slowest:");
    for (dur, size, path) in timings.iter().take(15) {
        println!("{:>10?}  {:>10} bytes  {}", dur, size, path.display());
    }
    let over_10ms = timings.iter().filter(|t| t.0.as_millis() > 10).count();
    let sum_over_10ms: std::time::Duration = timings
        .iter()
        .filter(|t| t.0.as_millis() > 10)
        .map(|t| t.0)
        .sum();
    println!("files >10ms: {over_10ms}, summing to {:?}", sum_over_10ms);
}

fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walkdir(&path));
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    out
}
