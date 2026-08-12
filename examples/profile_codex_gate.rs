//! One-off real-corpus profiler for the Codex discovery stages. Read-only.
use std::time::Instant;

fn main() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let codex_home = home.join(".codex");
    let bounds = resume::preview::jsonl::Bounds::default();
    let canonical_root = codex_home.canonicalize().unwrap();

    let t0 = Instant::now();
    let roots = resume::integration::codex::rollout_roots(&codex_home);
    let mut files = Vec::new();
    for root in &roots {
        files.extend(walkdir(&root.path));
    }
    files.sort();
    println!("list {} files: {:?}", files.len(), t0.elapsed());

    // Stage: gate-style first-record read only (max_records = 1).
    let gate_bounds = resume::preview::jsonl::Bounds {
        max_records: 1,
        ..bounds.clone()
    };
    let t1 = Instant::now();
    let mut metas = 0usize;
    for path in &files {
        if let Ok(read) =
            resume::preview::jsonl::read_file_confined(path, &canonical_root, &gate_bounds)
            && !read.records.is_empty()
        {
            metas += 1;
        }
    }
    println!("first-record read all ({metas} parsed): {:?}", t1.elapsed());

    // Stage: exact gate bounds (max_records = 1 AND 64 KiB byte cap).
    let exact_gate_bounds = resume::preview::jsonl::Bounds {
        max_records: 1,
        max_file_bytes: 64 * 1024,
        ..bounds.clone()
    };
    let t1b = Instant::now();
    let mut metas2 = 0usize;
    for path in &files {
        if let Ok(read) =
            resume::preview::jsonl::read_file_confined(path, &canonical_root, &exact_gate_bounds)
            && !read.records.is_empty()
        {
            metas2 += 1;
        }
    }
    println!(
        "first-record read 64KiB-capped ({metas2} parsed): {:?}",
        t1b.elapsed()
    );
    // Stage: full current parse (64KiB ladder).
    let t2 = Instant::now();
    let mut parsed_n = 0usize;
    for path in &files {
        if let Ok(Some(_)) =
            resume::integration::codex::parse_rollout_file(path, &canonical_root, &bounds)
        {
            parsed_n += 1;
        }
    }
    println!(
        "full parse_rollout_file all ({parsed_n} sessions): {:?}",
        t2.elapsed()
    );

    // Stage: gated with reject-all (what discovery pays per out-of-scope file).
    let gate = |_: &std::path::Path| false;
    let t3 = Instant::now();
    let mut kept = 0usize;
    for path in &files {
        if let Ok(Some(_)) = resume::integration::codex::parse_rollout_file_gated(
            path,
            &canonical_root,
            &bounds,
            Some(&gate),
        ) {
            kept += 1;
        }
    }
    println!("gated reject-all ({kept} kept): {:?}", t3.elapsed());
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
