//! Benchmark: bergamot-par2 vs par2cmdline
//!
//! Usage: cargo bench --bench vs_par2cmdline -- [iterations] [file_size] [file_count]
//!   e.g.: cargo bench --bench vs_par2cmdline -- 5 10m 5
//!
//! ## Baseline (2026-02-07, Apple M4 Pro, par2cmdline 0.8.1, rustc 1.92.0)
//!
//! | Operation | Data Size | par2cmdline | bergamot-par2 | Speedup |
//! |-----------|-----------|-------------|-----------|---------|
//! | Verify    |     5 MB  |       47 ms |    2.4 ms |   ~19x  |
//! | Verify    |    50 MB  |      430 ms |     20 ms |   ~21x  |
//! | Repair    |     5 MB  |       66 ms |     26 ms |  ~2.5x  |
//! | Repair    |    50 MB  |      584 ms |    212 ms | ~2.75x  |
//!
//! ## After #1: eliminate per-slice file open/alloc in repair
//!
//! | Operation | Data Size | par2cmdline | bergamot-par2 | Speedup |
//! |-----------|-----------|-------------|-----------|---------|
//! | Repair    |     5 MB  |       66 ms |     25 ms |  ~2.6x  |
//! | Repair    |    50 MB  |      583 ms |    209 ms |  ~2.8x  |
//!
//! ## After #2: hoist MulTable in gauss_eliminate
//!
//! | Operation | Data Size | par2cmdline | bergamot-par2 | Speedup |
//! |-----------|-----------|-------------|-----------|---------|
//! | Repair    |     5 MB  |       65 ms |     25 ms |  ~2.6x  |
//! | Repair    |    50 MB  |      583 ms |    213 ms |  ~2.7x  |
//!
//! ## After #3: NEON SIMD for muladd/mul_slice_inplace
//!
//! | Operation | Data Size | par2cmdline | bergamot-par2 | Speedup |
//! |-----------|-----------|-------------|-----------|---------|
//! | Repair    |     5 MB  |       65 ms |     14 ms |  ~4.7x  |
//! | Repair    |    50 MB  |      582 ms |    100 ms |  ~5.8x  |
//!
//! ## After #4: parallelize repair encoding loop with rayon
//!
//! | Operation | Data Size | par2cmdline | bergamot-par2 | Speedup |
//! |-----------|-----------|-------------|-----------|---------|
//! | Repair    |     5 MB  |       66 ms |     12 ms |  ~5.6x  |
//! | Repair    |    50 MB  |      584 ms |     55 ms | ~10.7x  |

use std::path::Path;
use std::process::Command;
use std::time::Instant;

fn par2_cli(args: &[&str], dir: &Path) -> (std::time::Duration, bool) {
    let start = Instant::now();
    let output = Command::new("par2")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run par2");
    let elapsed = start.elapsed();
    (elapsed, output.status.success())
}

fn generate_test_files(dir: &Path, count: usize, size: usize) {
    for i in 0..count {
        let data: Vec<u8> = (0..size).map(|b| ((b * (i + 1)) % 251) as u8).collect();
        std::fs::write(dir.join(format!("file_{i}.dat")), &data).unwrap();
    }
}

fn create_par2_files(dir: &Path) {
    let output = Command::new("par2")
        .args(["create", "-r30", "-b100", "bench.par2"])
        .args(
            std::fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| {
                    let e = e.ok()?;
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.ends_with(".dat") {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
        )
        .current_dir(dir)
        .output()
        .expect("par2 create failed");
    assert!(
        output.status.success(),
        "par2 create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bench_verify(dir: &Path, iterations: u32) {
    println!("\n=== VERIFY BENCHMARK ({iterations} iterations) ===\n");

    // Warm up: parse once
    let rs = bergamot_par2::parse_recovery_set(dir).expect("parse failed");
    println!(
        "  Files: {}, Slice size: {}, Recovery slices: {}",
        rs.files.len(),
        rs.slice_size,
        rs.recovery_slice_count
    );

    let total_data: u64 = rs.files.iter().map(|f| f.length).sum();
    println!("  Total data: {:.2} MB", total_data as f64 / 1_048_576.0);

    // par2cmdline verify
    let mut cli_times = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        let (elapsed, ok) = par2_cli(&["verify", "bench.par2"], dir);
        assert!(ok, "par2 CLI verify failed");
        cli_times.push(elapsed);
    }

    // native verify
    let mut native_times = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        let start = Instant::now();
        let result = bergamot_par2::verify_recovery_set(&rs, dir);
        let elapsed = start.elapsed();
        assert!(result.all_ok(), "native verify failed");
        native_times.push(elapsed);
    }

    report("par2cmdline verify", &cli_times);
    report("bergamot-par2   verify", &native_times);
    report_speedup(&cli_times, &native_times);
}

fn bench_repair(dir: &Path, iterations: u32) {
    println!("\n=== REPAIR BENCHMARK ({iterations} iterations) ===\n");

    let rs = bergamot_par2::parse_recovery_set(dir).expect("parse failed");

    // Figure out how many slices file_0 occupies so we only delete what's repairable
    let file0_entry = rs
        .files
        .iter()
        .find(|f| f.filename == "file_0.dat")
        .unwrap();
    let file0_slices = file0_entry.length.div_ceil(rs.slice_size) as usize;
    if file0_slices > rs.recovery_slice_count {
        println!(
            "  SKIPPED: file_0.dat needs {file0_slices} recovery slices, only {} available",
            rs.recovery_slice_count
        );
        return;
    }
    println!(
        "  Deleting file_0.dat ({file0_slices} slices, {} recovery slices available)",
        rs.recovery_slice_count
    );

    let file0_data = std::fs::read(dir.join("file_0.dat")).unwrap();

    // par2cmdline repair
    let mut cli_times = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        std::fs::remove_file(dir.join("file_0.dat")).unwrap();

        let (elapsed, ok) = par2_cli(&["repair", "bench.par2"], dir);
        assert!(ok, "par2 CLI repair failed");
        cli_times.push(elapsed);

        // Verify it was restored
        assert_eq!(std::fs::read(dir.join("file_0.dat")).unwrap(), file0_data);
    }

    // native repair
    let mut native_times = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        std::fs::remove_file(dir.join("file_0.dat")).unwrap();

        let start = Instant::now();
        let verify_result = bergamot_par2::verify_recovery_set(&rs, dir);
        bergamot_par2::repair_recovery_set(&rs, &verify_result, dir).expect("native repair failed");
        let elapsed = start.elapsed();
        native_times.push(elapsed);

        assert_eq!(std::fs::read(dir.join("file_0.dat")).unwrap(), file0_data);
    }

    report("par2cmdline repair", &cli_times);
    report("bergamot-par2   repair", &native_times);
    report_speedup(&cli_times, &native_times);
}

fn report(label: &str, times: &[std::time::Duration]) {
    let ms: Vec<f64> = times.iter().map(|t| t.as_secs_f64() * 1000.0).collect();
    let min = ms.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = ms.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    let median = {
        let mut sorted = ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted[sorted.len() / 2]
    };
    println!(
        "  {label}: median={median:8.2}ms  mean={mean:8.2}ms  min={min:8.2}ms  max={max:8.2}ms"
    );
}

fn report_speedup(cli: &[std::time::Duration], native: &[std::time::Duration]) {
    let cli_median = {
        let mut ms: Vec<f64> = cli.iter().map(|t| t.as_secs_f64()).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ms[ms.len() / 2]
    };
    let native_median = {
        let mut ms: Vec<f64> = native.iter().map(|t| t.as_secs_f64()).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ms[ms.len() / 2]
    };
    let ratio = cli_median / native_median;
    if ratio >= 1.0 {
        println!("  => bergamot-par2 is {ratio:.2}x faster than par2cmdline");
    } else {
        println!(
            "  => bergamot-par2 is {:.2}x slower than par2cmdline",
            1.0 / ratio
        );
    }
}

fn main() {
    let iterations: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let file_size: usize = std::env::args()
        .nth(2)
        .and_then(|s| {
            let s = s.to_lowercase();
            if let Some(mb) = s.strip_suffix("m") {
                mb.parse::<usize>().ok().map(|n| n * 1_048_576)
            } else if let Some(kb) = s.strip_suffix("k") {
                kb.parse::<usize>().ok().map(|n| n * 1024)
            } else {
                s.parse().ok()
            }
        })
        .unwrap_or(1_048_576); // 1 MB default

    let file_count: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    println!("par2 performance benchmark: bergamot-par2 vs par2cmdline");
    println!("  iterations:  {iterations}");
    println!("  file size:   {:.2} MB", file_size as f64 / 1_048_576.0);
    println!("  file count:  {file_count}");

    let dir = tempfile::tempdir().unwrap();
    println!("  working dir: {}", dir.path().display());

    print!("\nGenerating test files...");
    generate_test_files(dir.path(), file_count, file_size);
    println!(" done");

    print!("Creating PAR2 files with par2cmdline...");
    create_par2_files(dir.path());
    let par2_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".par2") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    println!(" done ({} par2 files)", par2_files.len());

    bench_verify(dir.path(), iterations);
    bench_repair(dir.path(), iterations);
}
