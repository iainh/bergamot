use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::Par2ParseError;
use crate::format::{
    FileDescriptionBody, HEADER_SIZE, IFSCBody, MainBody, PacketHeader, PacketType,
};
use crate::model::{FileId, Md5Digest, Par2FileEntry, RecoverySet, RecoverySliceRef};

struct RawParsed {
    set_id: Option<[u8; 16]>,
    main: Option<MainBody>,
    descriptions: Vec<FileDescriptionBody>,
    ifscs: Vec<IFSCBody>,
    recovery_slice_count: usize,
    recovery_slices: Vec<RecoverySliceRef>,
}

impl RawParsed {
    fn new() -> Self {
        Self {
            set_id: None,
            main: None,
            descriptions: Vec::new(),
            ifscs: Vec::new(),
            recovery_slice_count: 0,
            recovery_slices: Vec::new(),
        }
    }
}

fn parse_file<R: Read + Seek>(
    reader: &mut R,
    raw: &mut RawParsed,
    file_path: Option<&Path>,
) -> Result<(), Par2ParseError> {
    let mut offset: u64 = 0;
    let file_len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    let mut header_buf = [0u8; HEADER_SIZE];
    loop {
        if offset + HEADER_SIZE as u64 > file_len {
            break;
        }
        reader.seek(SeekFrom::Start(offset))?;
        if reader.read_exact(&mut header_buf).is_err() {
            break;
        }
        let header = match PacketHeader::from_bytes(&header_buf) {
            Some(h) => h,
            None => {
                break;
            }
        };

        if offset + header.length > file_len {
            break;
        }

        if let Some(existing) = raw.set_id {
            if existing != header.set_id {
                return Err(Par2ParseError::InconsistentSetId);
            }
        } else {
            raw.set_id = Some(header.set_id);
        }

        let ptype = PacketType::from_bytes(&header.packet_type);
        let body_len = header.body_length() as usize;

        match ptype {
            PacketType::Main => {
                let mut body = vec![0u8; body_len];
                reader.read_exact(&mut body)?;
                if let Some(main) = MainBody::from_bytes(&body) {
                    raw.main = Some(main);
                }
            }
            PacketType::FileDescription => {
                let mut body = vec![0u8; body_len];
                reader.read_exact(&mut body)?;
                if let Some(desc) = FileDescriptionBody::from_bytes(&body) {
                    raw.descriptions.push(desc);
                }
            }
            PacketType::IFSC => {
                let mut body = vec![0u8; body_len];
                reader.read_exact(&mut body)?;
                if let Some(ifsc) = IFSCBody::from_bytes(&body) {
                    raw.ifscs.push(ifsc);
                }
            }
            PacketType::RecoverySlice => {
                raw.recovery_slice_count += 1;
                if body_len >= 4 {
                    let mut exp_buf = [0u8; 4];
                    reader.read_exact(&mut exp_buf)?;
                    let exponent = u32::from_le_bytes(exp_buf);
                    let data_offset = offset + HEADER_SIZE as u64 + 4;
                    let data_len = body_len - 4;
                    if let Some(path) = file_path {
                        raw.recovery_slices.push(RecoverySliceRef {
                            par2_path: path.to_path_buf(),
                            data_offset,
                            data_len,
                            exponent,
                        });
                    }
                }
            }
            PacketType::Creator | PacketType::Unknown => {}
        }

        offset += header.length;
    }
    Ok(())
}

fn build_recovery_set(raw: RawParsed) -> Result<RecoverySet, Par2ParseError> {
    let main = raw.main.ok_or(Par2ParseError::NoMainPacket)?;
    let set_id = raw.set_id.ok_or(Par2ParseError::NoMainPacket)?;

    let mut files = Vec::with_capacity(main.recoverable_file_count as usize);

    for &fid in &main.file_ids[..main.recoverable_file_count as usize] {
        let desc = raw.descriptions.iter().find(|d| d.file_id == fid);
        let ifsc = raw.ifscs.iter().find(|i| i.file_id == fid);

        if let Some(desc) = desc {
            let filename = String::from_utf8_lossy(&desc.name).into_owned();
            files.push(Par2FileEntry {
                file_id: FileId(fid),
                filename,
                length: desc.length,
                hash_full: Md5Digest(desc.hash_full),
                hash_16k: Md5Digest(desc.hash_16k),
                slice_checksums: ifsc.map_or_else(Vec::new, |i| i.entries.clone()),
            });
        }
    }

    Ok(RecoverySet {
        set_id,
        slice_size: main.slice_size,
        files,
        recovery_slice_count: raw.recovery_slice_count,
        recovery_slices: raw.recovery_slices,
    })
}

pub fn parse_recovery_set(par2_dir: &Path) -> Result<RecoverySet, Par2ParseError> {
    let mut raw = RawParsed::new();

    let entries: Vec<_> = std::fs::read_dir(par2_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("par2"))
        })
        .collect();

    for entry in entries {
        let path = entry.path();
        let mut file = std::fs::File::open(&path)?;
        parse_file(&mut file, &mut raw, Some(&path))?;
    }

    build_recovery_set(raw)
}

pub fn parse_recovery_set_from_file(par2_file: &Path) -> Result<RecoverySet, Par2ParseError> {
    let mut raw = RawParsed::new();
    let mut file = std::fs::File::open(par2_file)?;
    parse_file(&mut file, &mut raw, Some(par2_file))?;
    build_recovery_set(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{HEADER_SIZE, MAGIC};
    use std::io::Cursor;

    fn make_packet(set_id: &[u8; 16], ptype: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let total_len = HEADER_SIZE as u64 + body.len() as u64;
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&MAGIC);
        pkt.extend_from_slice(&total_len.to_le_bytes());
        pkt.extend_from_slice(&[0u8; 16]); // hash (unchecked for now)
        pkt.extend_from_slice(set_id);
        pkt.extend_from_slice(ptype);
        pkt.extend_from_slice(body);
        pkt
    }

    fn make_main_body(slice_size: u64, file_count: u32, file_ids: &[[u8; 16]]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&slice_size.to_le_bytes());
        body.extend_from_slice(&file_count.to_le_bytes());
        for fid in file_ids {
            body.extend_from_slice(fid);
        }
        body
    }

    fn make_file_desc_body(
        file_id: &[u8; 16],
        hash_full: &[u8; 16],
        hash_16k: &[u8; 16],
        length: u64,
        name: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(file_id);
        body.extend_from_slice(hash_full);
        body.extend_from_slice(hash_16k);
        body.extend_from_slice(&length.to_le_bytes());
        body.extend_from_slice(name);
        let padding = (4 - (name.len() % 4)) % 4;
        body.extend(std::iter::repeat_n(0u8, padding));
        body
    }

    fn make_ifsc_body(file_id: &[u8; 16], entries: &[([u8; 16], u32)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(file_id);
        for (md5, crc) in entries {
            body.extend_from_slice(md5);
            body.extend_from_slice(&crc.to_le_bytes());
        }
        body
    }

    #[test]
    fn parse_single_file_recovery_set() {
        let set_id = [0x42u8; 16];
        let file_id = [0xAA; 16];

        let main_body = make_main_body(1024, 1, &[file_id]);
        let desc_body = make_file_desc_body(&file_id, &[0xBB; 16], &[0xCC; 16], 2048, b"test.rar");
        let ifsc_body = make_ifsc_body(&file_id, &[([0x11; 16], 100), ([0x22; 16], 200)]);

        let mut data = Vec::new();
        data.extend(make_packet(&set_id, b"PAR 2.0\0Main\0\0\0\0", &main_body));
        data.extend(make_packet(&set_id, b"PAR 2.0\0FileDesc", &desc_body));
        data.extend(make_packet(&set_id, b"PAR 2.0\0IFSC\0\0\0\0", &ifsc_body));
        data.extend(make_packet(&set_id, b"PAR 2.0\0Creator\0", b"nzbg"));

        let mut cursor = Cursor::new(data);
        let mut raw = RawParsed::new();
        parse_file(&mut cursor, &mut raw, None).unwrap();

        let rs = build_recovery_set(raw).unwrap();
        assert_eq!(rs.set_id, set_id);
        assert_eq!(rs.slice_size, 1024);
        assert_eq!(rs.files.len(), 1);
        assert_eq!(rs.files[0].filename, "test.rar");
        assert_eq!(rs.files[0].length, 2048);
        assert_eq!(rs.files[0].slice_checksums.len(), 2);
        assert_eq!(rs.files[0].slice_checksums[0].crc32, 100);
        assert_eq!(rs.recovery_slice_count, 0);
    }

    #[test]
    fn parse_counts_recovery_slices() {
        let set_id = [0x42u8; 16];
        let file_id = [0xAA; 16];

        let main_body = make_main_body(512, 1, &[file_id]);
        let desc_body = make_file_desc_body(&file_id, &[0; 16], &[0; 16], 512, b"f.dat");

        // Recovery slice: body is exponent (4 bytes) + recovery data (must be 4-aligned)
        let mut recovery_body = vec![0u8; 4]; // exponent
        recovery_body.extend(vec![0u8; 512]); // recovery data

        let mut data = Vec::new();
        data.extend(make_packet(&set_id, b"PAR 2.0\0Main\0\0\0\0", &main_body));
        data.extend(make_packet(&set_id, b"PAR 2.0\0FileDesc", &desc_body));
        data.extend(make_packet(&set_id, b"PAR 2.0\0RecvSlic", &recovery_body));
        data.extend(make_packet(&set_id, b"PAR 2.0\0RecvSlic", &recovery_body));

        let mut cursor = Cursor::new(data);
        let mut raw = RawParsed::new();
        parse_file(&mut cursor, &mut raw, None).unwrap();

        let rs = build_recovery_set(raw).unwrap();
        assert_eq!(rs.recovery_slice_count, 2);
    }

    #[test]
    fn parse_rejects_inconsistent_set_id() {
        let set_id_a = [0x01u8; 16];
        let set_id_b = [0x02u8; 16];
        let file_id = [0xAA; 16];

        let main_body = make_main_body(512, 1, &[file_id]);
        let desc_body = make_file_desc_body(&file_id, &[0; 16], &[0; 16], 512, b"f.dat");

        let mut data = Vec::new();
        data.extend(make_packet(&set_id_a, b"PAR 2.0\0Main\0\0\0\0", &main_body));
        data.extend(make_packet(&set_id_b, b"PAR 2.0\0FileDesc", &desc_body));

        let mut cursor = Cursor::new(data);
        let mut raw = RawParsed::new();
        let err = parse_file(&mut cursor, &mut raw, None).unwrap_err();
        assert!(matches!(err, Par2ParseError::InconsistentSetId));
    }

    #[test]
    fn parse_from_directory_finds_par2_files() {
        let dir = tempfile::tempdir().unwrap();
        let set_id = [0x42u8; 16];
        let file_id = [0xAA; 16];

        let main_body = make_main_body(256, 1, &[file_id]);
        let desc_body = make_file_desc_body(&file_id, &[0; 16], &[0; 16], 256, b"data.bin");
        let ifsc_body = make_ifsc_body(&file_id, &[([0x11; 16], 42)]);

        let mut par2_data = Vec::new();
        par2_data.extend(make_packet(&set_id, b"PAR 2.0\0Main\0\0\0\0", &main_body));
        par2_data.extend(make_packet(&set_id, b"PAR 2.0\0FileDesc", &desc_body));
        par2_data.extend(make_packet(&set_id, b"PAR 2.0\0IFSC\0\0\0\0", &ifsc_body));
        par2_data.extend(make_packet(&set_id, b"PAR 2.0\0Creator\0", b"test"));

        std::fs::write(dir.path().join("test.par2"), &par2_data).unwrap();
        std::fs::write(dir.path().join("notpar.txt"), b"ignored").unwrap();

        let rs = parse_recovery_set(dir.path()).unwrap();
        assert_eq!(rs.files.len(), 1);
        assert_eq!(rs.files[0].filename, "data.bin");
    }
}
