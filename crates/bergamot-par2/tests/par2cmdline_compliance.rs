use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_available(name: &str) -> bool {
    fixtures_dir().join(name).exists()
}

fn require_fixture(name: &str) {
    if !fixture_available(name) {
        panic!(
            "Fixture {name} not found. Run: \
             cd crates/bergamot-par2/tests/fixtures && \
             curl -sLO https://raw.githubusercontent.com/Parchive/par2cmdline/master/tests/{name}"
        );
    }
}

fn extract_tarball(tarball: &str, dest: &Path) {
    require_fixture(tarball);
    let status = Command::new("tar")
        .args(["xzf", &fixtures_dir().join(tarball).to_string_lossy()])
        .current_dir(dest)
        .status()
        .expect("tar failed");
    assert!(status.success(), "tar extract failed for {tarball}");
}

fn par2_available() -> bool {
    Command::new("par2")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Phase 4a: flatdata corpus – basic PAR2 verification ──

#[test]
fn compliance_flatdata_verify_all_ok() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");

    assert_eq!(rs.files.len(), 10, "expected 10 files in flatdata set");
    assert!(rs.slice_size > 0);

    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(result.all_ok(), "all files should verify OK: {result:?}");
    assert_eq!(result.blocks_needed(), 0);
}

#[test]
fn compliance_flatdata_detect_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    std::fs::remove_file(dir.path().join("test-1.data")).unwrap();
    std::fs::remove_file(dir.path().join("test-3.data")).unwrap();

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());

    assert!(!result.all_ok());
    assert!(result.blocks_needed() > 0);

    let missing: Vec<_> = result
        .files
        .iter()
        .filter(|f| f.status == bergamot_par2::FileVerifyStatus::Missing)
        .map(|f| f.filename.as_str())
        .collect();
    assert!(
        missing.contains(&"test-1.data"),
        "test-1.data should be missing"
    );
    assert!(
        missing.contains(&"test-3.data"),
        "test-3.data should be missing"
    );
}

#[test]
fn compliance_flatdata_detect_damaged_file() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    let path = dir.path().join("test-5.data");
    let mut data = std::fs::read(&path).unwrap();
    data[0] ^= 0xFF;
    std::fs::write(&path, &data).unwrap();

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());

    assert!(!result.all_ok());
    let f5 = result
        .files
        .iter()
        .find(|f| f.filename == "test-5.data")
        .expect("test-5.data should be in results");
    assert!(
        matches!(f5.status, bergamot_par2::FileVerifyStatus::Damaged { .. }),
        "expected Damaged for test-5.data, got: {:?}",
        f5.status
    );
}

#[test]
fn compliance_flatdata_recovery_slice_count() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    assert!(
        rs.recovery_slice_count > 0,
        "should have recovery slices from .vol files"
    );
}

// ── Phase 4b: cross-check with par2 CLI ──

#[test]
fn compliance_flatdata_cross_check_with_cli() {
    if !par2_available() {
        eprintln!("SKIPPED: par2 CLI not installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    // CLI verify
    let output = Command::new("par2")
        .args(["verify", "testdata.par2"])
        .current_dir(dir.path())
        .output()
        .expect("par2 verify failed");
    let cli_ok = String::from_utf8_lossy(&output.stdout).contains("All files are correct");
    assert!(cli_ok, "par2 CLI should report all files correct");

    // Native verify
    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(result.all_ok(), "native engine should agree with CLI");
}

#[test]
fn compliance_flatdata_cross_check_damaged_with_cli() {
    if !par2_available() {
        eprintln!("SKIPPED: par2 CLI not installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    // Delete two files (same as par2cmdline test4)
    std::fs::remove_file(dir.path().join("test-1.data")).unwrap();
    std::fs::remove_file(dir.path().join("test-3.data")).unwrap();

    // CLI verify — should report repair needed
    let output = Command::new("par2")
        .args(["verify", "testdata.par2"])
        .current_dir(dir.path())
        .output()
        .expect("par2 verify failed");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("Repair is required") || combined.contains("repair is required"),
        "par2 CLI should report repair needed, got: {combined}"
    );

    // Native verify — should also report not-ok
    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(
        !result.all_ok(),
        "native engine should detect missing files"
    );
    assert!(
        result.blocks_needed() > 0,
        "native engine should report blocks needed"
    );
}

// ── Phase 4c: readbeyondeof edge case ──

#[test]
fn compliance_readbeyondeof_verify_ok() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("readbeyondeof.tar.gz", dir.path());

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    assert_eq!(rs.files.len(), 1);

    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(
        result.all_ok(),
        "readbeyondeof fixture should verify OK: {result:?}"
    );
}

// ── Phase 4/6: repair tests ──

#[test]
fn compliance_flatdata_repair_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    std::fs::remove_file(dir.path().join("test-1.data")).unwrap();
    std::fs::remove_file(dir.path().join("test-3.data")).unwrap();

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(!result.all_ok());

    let report = bergamot_par2::repair_recovery_set(&rs, &result, dir.path()).expect("repair failed");
    assert!(report.repaired_slices > 0);

    let result_after = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(
        result_after.all_ok(),
        "files should verify OK after repair: {result_after:?}"
    );
}

#[test]
fn compliance_flatdata_repair_damaged_file() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    let path = dir.path().join("test-5.data");
    let mut data = std::fs::read(&path).unwrap();
    data[0] ^= 0xFF;
    std::fs::write(&path, &data).unwrap();

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(!result.all_ok());

    let report = bergamot_par2::repair_recovery_set(&rs, &result, dir.path()).expect("repair failed");
    assert!(report.repaired_slices > 0);

    let result_after = bergamot_par2::verify_recovery_set(&rs, dir.path());
    assert!(
        result_after.all_ok(),
        "files should verify OK after repair: {result_after:?}"
    );
}

#[test]
fn compliance_flatdata_repair_cross_check_with_cli() {
    if !par2_available() {
        eprintln!("SKIPPED: par2 CLI not installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    extract_tarball("flatdata.tar.gz", dir.path());
    extract_tarball("flatdata-par2files.tar.gz", dir.path());

    // Save originals for comparison
    let orig_1 = std::fs::read(dir.path().join("test-1.data")).unwrap();
    let orig_3 = std::fs::read(dir.path().join("test-3.data")).unwrap();

    std::fs::remove_file(dir.path().join("test-1.data")).unwrap();
    std::fs::remove_file(dir.path().join("test-3.data")).unwrap();

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());

    bergamot_par2::repair_recovery_set(&rs, &result, dir.path()).expect("repair failed");

    let repaired_1 = std::fs::read(dir.path().join("test-1.data")).unwrap();
    let repaired_3 = std::fs::read(dir.path().join("test-3.data")).unwrap();

    assert_eq!(
        repaired_1, orig_1,
        "repaired test-1.data must match original"
    );
    assert_eq!(
        repaired_3, orig_3,
        "repaired test-3.data must match original"
    );
}

// ── Phase 4d: subdirectory handling ──

#[test]
fn compliance_subdirdata_parse_with_subdirs() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("subdirdata.tar.gz", dir.path());
    extract_tarball("subdirdata-par2files-unix.tar.gz", dir.path());

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");

    assert_eq!(rs.files.len(), 10, "subdirdata set should have 10 files");
    assert!(rs.slice_size > 0);

    // File paths should include subdirectory prefixes
    let filenames: Vec<&str> = rs.files.iter().map(|f| f.filename.as_str()).collect();
    let has_subdir_paths = filenames
        .iter()
        .any(|f| f.contains("subdir1") || f.contains("subdir2"));
    assert!(
        has_subdir_paths,
        "subdirdata files should have subdirectory paths, got: {filenames:?}"
    );
}

#[test]
fn compliance_subdirdata_verify_all_ok() {
    let dir = tempfile::tempdir().unwrap();
    extract_tarball("subdirdata.tar.gz", dir.path());
    extract_tarball("subdirdata-par2files-unix.tar.gz", dir.path());

    let rs = bergamot_par2::parse_recovery_set(dir.path()).expect("parse failed");
    let result = bergamot_par2::verify_recovery_set(&rs, dir.path());

    assert!(result.all_ok(), "subdirdata should verify OK: {result:?}");
}
