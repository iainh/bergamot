pub const MAIN_TYPE: [u8; 16] = *b"PAR 2.0\0Main\0\0\0\0";
pub const FILE_DESCRIPTION_TYPE: [u8; 16] = *b"PAR 2.0\0FileDesc";
pub const IFSC_TYPE: [u8; 16] = *b"PAR 2.0\0IFSC\0\0\0\0";
pub const RECOVERY_SLICE_TYPE: [u8; 16] = *b"PAR 2.0\0RecvSlic";
pub const CREATOR_TYPE: [u8; 16] = *b"PAR 2.0\0Creator\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Main,
    FileDescription,
    IFSC,
    RecoverySlice,
    Creator,
    Unknown,
}

impl PacketType {
    pub fn from_bytes(raw: &[u8; 16]) -> Self {
        match raw {
            x if x == &MAIN_TYPE => Self::Main,
            x if x == &FILE_DESCRIPTION_TYPE => Self::FileDescription,
            x if x == &IFSC_TYPE => Self::IFSC,
            x if x == &RECOVERY_SLICE_TYPE => Self::RecoverySlice,
            x if x == &CREATOR_TYPE => Self::Creator,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainBody {
    pub slice_size: u64,
    pub recoverable_file_count: u32,
    pub file_ids: Vec<[u8; 16]>,
}

impl MainBody {
    pub fn from_bytes(body: &[u8]) -> Option<Self> {
        if body.len() < 12 {
            return None;
        }
        let slice_size = u64::from_le_bytes(body[..8].try_into().ok()?);
        let recoverable_file_count = u32::from_le_bytes(body[8..12].try_into().ok()?);
        let rest = &body[12..];
        if !rest.len().is_multiple_of(16) {
            return None;
        }
        let file_ids: Vec<[u8; 16]> = rest
            .chunks_exact(16)
            .map(|c| c.try_into().unwrap())
            .collect();
        Some(Self {
            slice_size,
            recoverable_file_count,
            file_ids,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptionBody {
    pub file_id: [u8; 16],
    pub hash_full: [u8; 16],
    pub hash_16k: [u8; 16],
    pub length: u64,
    pub name: Vec<u8>,
}

impl FileDescriptionBody {
    pub fn from_bytes(body: &[u8]) -> Option<Self> {
        if body.len() < 56 {
            return None;
        }
        let file_id: [u8; 16] = body[..16].try_into().ok()?;
        let hash_full: [u8; 16] = body[16..32].try_into().ok()?;
        let hash_16k: [u8; 16] = body[32..48].try_into().ok()?;
        let length = u64::from_le_bytes(body[48..56].try_into().ok()?);
        let name_bytes = &body[56..];
        let name = name_bytes.iter().copied().take_while(|&b| b != 0).collect();
        Some(Self {
            file_id,
            hash_full,
            hash_16k,
            length,
            name,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceChecksumEntry {
    pub md5: [u8; 16],
    pub crc32: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IFSCBody {
    pub file_id: [u8; 16],
    pub entries: Vec<SliceChecksumEntry>,
}

impl IFSCBody {
    pub fn from_bytes(body: &[u8]) -> Option<Self> {
        if body.len() < 16 {
            return None;
        }
        let file_id: [u8; 16] = body[..16].try_into().ok()?;
        let rest = &body[16..];
        if !rest.len().is_multiple_of(20) {
            return None;
        }
        let entries = rest
            .chunks_exact(20)
            .map(|c| SliceChecksumEntry {
                md5: c[..16].try_into().unwrap(),
                crc32: u32::from_le_bytes(c[16..20].try_into().unwrap()),
            })
            .collect();
        Some(Self { file_id, entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_type_from_bytes() {
        assert_eq!(PacketType::from_bytes(&MAIN_TYPE), PacketType::Main);
        assert_eq!(
            PacketType::from_bytes(&FILE_DESCRIPTION_TYPE),
            PacketType::FileDescription
        );
        assert_eq!(PacketType::from_bytes(&IFSC_TYPE), PacketType::IFSC);
        assert_eq!(
            PacketType::from_bytes(&RECOVERY_SLICE_TYPE),
            PacketType::RecoverySlice
        );
        assert_eq!(PacketType::from_bytes(&CREATOR_TYPE), PacketType::Creator);
        assert_eq!(
            PacketType::from_bytes(b"unknown_type____"),
            PacketType::Unknown
        );
    }

    #[test]
    fn parse_main_body() {
        let mut body = Vec::new();
        body.extend_from_slice(&1024u64.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(&[1u8; 16]);
        body.extend_from_slice(&[2u8; 16]);

        let main = MainBody::from_bytes(&body).unwrap();
        assert_eq!(main.slice_size, 1024);
        assert_eq!(main.recoverable_file_count, 2);
        assert_eq!(main.file_ids.len(), 2);
        assert_eq!(main.file_ids[0], [1u8; 16]);
        assert_eq!(main.file_ids[1], [2u8; 16]);
    }

    #[test]
    fn parse_main_body_rejects_short_input() {
        assert!(MainBody::from_bytes(&[0; 11]).is_none());
    }

    #[test]
    fn parse_main_body_rejects_unaligned_file_ids() {
        let mut body = Vec::new();
        body.extend_from_slice(&1024u64.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 15]);
        assert!(MainBody::from_bytes(&body).is_none());
    }

    #[test]
    fn parse_file_description_body() {
        let mut body = Vec::new();
        body.extend_from_slice(&[0xAA; 16]); // file_id
        body.extend_from_slice(&[0xBB; 16]); // hash_full
        body.extend_from_slice(&[0xCC; 16]); // hash_16k
        body.extend_from_slice(&12345u64.to_le_bytes()); // length
        body.extend_from_slice(b"test.rar\0\0\0\0"); // name, 4-aligned

        let desc = FileDescriptionBody::from_bytes(&body).unwrap();
        assert_eq!(desc.file_id, [0xAA; 16]);
        assert_eq!(desc.hash_full, [0xBB; 16]);
        assert_eq!(desc.hash_16k, [0xCC; 16]);
        assert_eq!(desc.length, 12345);
        assert_eq!(desc.name, b"test.rar");
    }

    #[test]
    fn parse_file_description_strips_null_padding() {
        let mut body = vec![0u8; 56];
        body.extend_from_slice(b"ab\0\0");
        let desc = FileDescriptionBody::from_bytes(&body).unwrap();
        assert_eq!(desc.name, b"ab");
    }

    #[test]
    fn parse_file_description_rejects_short_input() {
        assert!(FileDescriptionBody::from_bytes(&[0; 55]).is_none());
    }

    #[test]
    fn parse_ifsc_body() {
        let mut body = Vec::new();
        body.extend_from_slice(&[0xDD; 16]); // file_id

        // Two entries: each is 16-byte MD5 + 4-byte CRC32 = 20 bytes
        body.extend_from_slice(&[0x11; 16]);
        body.extend_from_slice(&42u32.to_le_bytes());
        body.extend_from_slice(&[0x22; 16]);
        body.extend_from_slice(&99u32.to_le_bytes());

        let ifsc = IFSCBody::from_bytes(&body).unwrap();
        assert_eq!(ifsc.file_id, [0xDD; 16]);
        assert_eq!(ifsc.entries.len(), 2);
        assert_eq!(ifsc.entries[0].md5, [0x11; 16]);
        assert_eq!(ifsc.entries[0].crc32, 42);
        assert_eq!(ifsc.entries[1].md5, [0x22; 16]);
        assert_eq!(ifsc.entries[1].crc32, 99);
    }

    #[test]
    fn parse_ifsc_body_rejects_unaligned_entries() {
        let mut body = vec![0u8; 16]; // file_id
        body.extend_from_slice(&[0u8; 19]); // incomplete entry
        assert!(IFSCBody::from_bytes(&body).is_none());
    }
}
