use std::time::Instant;

fn main() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let codex_home = home.join(".codex");
    let bounds = resume::preview::jsonl::Bounds::default();
    let canonical_root = codex_home.canonicalize().unwrap();

    let roots = resume::integration::codex::rollout_roots(&codex_home);
    let mut total_files = 0;
    let mut early_ok = 0;
    let mut errors = 0;
    let mut none_count = 0;

    let t0 = Instant::now();
    for root in &roots {
        for path in walkdir(&root.path) {
            total_files += 1;
            match resume::integration::codex::parse_rollout_file(&path, &canonical_root, &bounds) {
                Ok(Some(_)) => early_ok += 1,
                Ok(None) => none_count += 1,
                Err(_) => errors += 1,
            }
        }
    }
    let elapsed = t0.elapsed();
    println!("total files: {total_files}, ok: {early_ok}, none: {none_count}, errors: {errors}");
    println!("elapsed: {:?}", elapsed);
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
