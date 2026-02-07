use std::process::Command;

use digest::Digest;

fn par2_available() -> bool {
    Command::new("par2")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn compute_md5(data: &[u8]) -> [u8; 16] {
    let mut h = md5::Md5::new();
    h.update(data);
    h.finalize().into()
}

#[test]
fn end_to_end_with_par2_cli() {
    if !par2_available() {
        eprintln!("SKIPPED: par2 CLI not installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();

    // Create test data files
    let file1_data: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
    let file2_data: Vec<u8> = (0..5_000).map(|i| (i % 173) as u8).collect();
    std::fs::write(dir.path().join("file1.dat"), &file1_data).unwrap();
    std::fs::write(dir.path().join("file2.dat"), &file2_data).unwrap();

    // Create PAR2 files using par2 CLI
    let output = Command::new("par2")
        .args(["create", "-r10", "-b10"])
        .arg(dir.path().join("test.par2"))
        .arg(dir.path().join("file1.dat"))
        .arg(dir.path().join("file2.dat"))
        .output()
        .expect("par2 create failed");
    assert!(
        output.status.success(),
        "par2 create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse the PAR2 files with our engine
    let rs = nzbg_par2::parse_recovery_set(dir.path()).expect("parse failed");

    assert!(rs.slice_size > 0);
    assert_eq!(rs.files.len(), 2);

    // Verify — all files should be OK
    let result = nzbg_par2::verify_recovery_set(&rs, dir.path());
    assert!(result.all_ok(), "expected all OK, got: {result:?}");

    // Now damage a file and re-verify
    let mut damaged = file1_data.clone();
    damaged[500] ^= 0xFF;
    std::fs::write(dir.path().join("file1.dat"), &damaged).unwrap();

    let result = nzbg_par2::verify_recovery_set(&rs, dir.path());
    assert!(!result.all_ok());
    let f1_result = result
        .files
        .iter()
        .find(|f| f.filename == "file1.dat")
        .unwrap();
    assert!(
        matches!(
            f1_result.status,
            nzbg_par2::FileVerifyStatus::Damaged { .. }
        ),
        "expected Damaged for file1.dat, got: {:?}",
        f1_result.status
    );

    // Delete a file and verify
    std::fs::remove_file(dir.path().join("file2.dat")).unwrap();
    let result = nzbg_par2::verify_recovery_set(&rs, dir.path());
    let f2_result = result
        .files
        .iter()
        .find(|f| f.filename == "file2.dat")
        .unwrap();
    assert_eq!(f2_result.status, nzbg_par2::FileVerifyStatus::Missing);
}

#[test]
fn end_to_end_synthetic_par2() {
    use nzbg_par2::format::{HEADER_SIZE, MAGIC};

    let dir = tempfile::tempdir().unwrap();

    let file_data = vec![0x42u8; 200];
    let slice_size = 128u64;
    std::fs::write(dir.path().join("payload.bin"), &file_data).unwrap();

    // Build checksums for the file
    let full_md5 = compute_md5(&file_data);
    let hash_16k = compute_md5(&file_data);

    // Compute file_id = MD5(hash_16k || length_le || name_bytes)
    let mut file_id_input = Vec::new();
    file_id_input.extend_from_slice(&hash_16k);
    file_id_input.extend_from_slice(&(file_data.len() as u64).to_le_bytes());
    file_id_input.extend_from_slice(b"payload.bin");
    let file_id = compute_md5(&file_id_input);

    // Compute slice checksums (2 slices: 128 + 72 bytes)
    let slice1 = &file_data[..128];
    let slice1_md5 = compute_md5(slice1);
    let slice1_crc = crc32fast::hash(slice1);

    let mut slice2_padded = file_data[128..].to_vec();
    slice2_padded.resize(128, 0);
    let slice2_md5 = compute_md5(&slice2_padded);
    let slice2_crc = crc32fast::hash(&slice2_padded);

    // Build packets
    fn make_packet(set_id: &[u8; 16], ptype: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let total_len = HEADER_SIZE as u64 + body.len() as u64;
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&MAGIC);
        pkt.extend_from_slice(&total_len.to_le_bytes());
        pkt.extend_from_slice(&[0u8; 16]); // hash placeholder
        pkt.extend_from_slice(set_id);
        pkt.extend_from_slice(ptype);
        pkt.extend_from_slice(body);
        pkt
    }

    let set_id = [0x99u8; 16];

    // Main packet
    let mut main_body = Vec::new();
    main_body.extend_from_slice(&slice_size.to_le_bytes());
    main_body.extend_from_slice(&1u32.to_le_bytes());
    main_body.extend_from_slice(&file_id);
    let main_pkt = make_packet(&set_id, b"PAR 2.0\0Main\0\0\0\0", &main_body);

    // File description packet
    let mut desc_body = Vec::new();
    desc_body.extend_from_slice(&file_id);
    desc_body.extend_from_slice(&full_md5);
    desc_body.extend_from_slice(&hash_16k);
    desc_body.extend_from_slice(&(file_data.len() as u64).to_le_bytes());
    desc_body.extend_from_slice(b"payload.bin\0"); // null-padded to 4-align (11+1=12)
    let desc_pkt = make_packet(&set_id, b"PAR 2.0\0FileDesc", &desc_body);

    // IFSC packet
    let mut ifsc_body = Vec::new();
    ifsc_body.extend_from_slice(&file_id);
    ifsc_body.extend_from_slice(&slice1_md5);
    ifsc_body.extend_from_slice(&slice1_crc.to_le_bytes());
    ifsc_body.extend_from_slice(&slice2_md5);
    ifsc_body.extend_from_slice(&slice2_crc.to_le_bytes());
    let ifsc_pkt = make_packet(&set_id, b"PAR 2.0\0IFSC\0\0\0\0", &ifsc_body);

    // Creator packet
    let creator_pkt = make_packet(&set_id, b"PAR 2.0\0Creator\0", b"nzbg-par2-test\0\0");

    // Write .par2 file
    let mut par2_data = Vec::new();
    par2_data.extend(&main_pkt);
    par2_data.extend(&desc_pkt);
    par2_data.extend(&ifsc_pkt);
    par2_data.extend(&creator_pkt);
    std::fs::write(dir.path().join("payload.par2"), &par2_data).unwrap();

    // Parse and verify
    let rs = nzbg_par2::parse_recovery_set(dir.path()).expect("parse failed");
    assert_eq!(rs.files.len(), 1);
    assert_eq!(rs.files[0].filename, "payload.bin");
    assert_eq!(rs.files[0].length, 200);
    assert_eq!(rs.files[0].slice_checksums.len(), 2);

    let result = nzbg_par2::verify_recovery_set(&rs, dir.path());
    assert!(result.all_ok(), "verification failed: {result:?}");
}
