use std::time::Instant;

fn main() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let codex_home = home.join(".codex");
    let bounds = resume::preview::jsonl::Bounds::default();

    let t0 = Instant::now();
    let out = resume::integration::codex::discover(&codex_home, &bounds);
    let discover_time = t0.elapsed();
    println!("discover(): {:?} for {} outcomes", discover_time, out.len());

    // Second run (warm cache)
    let t1 = Instant::now();
    let out2 = resume::integration::codex::discover(&codex_home, &bounds);
    let discover_time2 = t1.elapsed();
    println!(
        "discover() 2nd run: {:?} for {} outcomes",
        discover_time2,
        out2.len()
    );

    // File listing + stat only
    let t2 = Instant::now();
    let roots = resume::integration::codex::rollout_roots(&codex_home);
    let mut total_files = 0;
    let mut total_bytes = 0u64;
    for root in &roots {
        for entry in walkdir(&root.path) {
            total_files += 1;
            if let Ok(meta) = std::fs::metadata(&entry) {
                total_bytes += meta.len();
            }
        }
    }
    let list_time = t2.elapsed();
    println!(
        "file listing + stat: {:?} for {} files, {:.1} MB total",
        list_time,
        total_files,
        total_bytes as f64 / 1e6
    );
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
