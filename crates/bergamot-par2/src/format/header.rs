pub const MAGIC: [u8; 8] = *b"PAR2\0PKT";
pub const HEADER_SIZE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketHeader {
    pub length: u64,
    pub hash: [u8; 16],
    pub set_id: [u8; 16],
    pub packet_type: [u8; 16],
}

impl PacketHeader {
    pub fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Option<Self> {
        if buf[..8] != MAGIC {
            return None;
        }
        let length = u64::from_le_bytes(buf[8..16].try_into().ok()?);
        if length < HEADER_SIZE as u64 || length % 4 != 0 {
            return None;
        }
        let hash: [u8; 16] = buf[16..32].try_into().ok()?;
        let set_id: [u8; 16] = buf[32..48].try_into().ok()?;
        let packet_type: [u8; 16] = buf[48..64].try_into().ok()?;
        Some(Self {
            length,
            hash,
            set_id,
            packet_type,
        })
    }

    pub fn body_length(&self) -> u64 {
        self.length - HEADER_SIZE as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_header(length: u64, packet_type: &[u8; 16]) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[..8].copy_from_slice(&MAGIC);
        buf[8..16].copy_from_slice(&length.to_le_bytes());
        buf[48..64].copy_from_slice(packet_type);
        buf
    }

    #[test]
    fn parse_valid_header() {
        let buf = make_header(64, b"PAR 2.0\0Main\0\0\0\0");
        let hdr = PacketHeader::from_bytes(&buf).unwrap();
        assert_eq!(hdr.length, 64);
        assert_eq!(&hdr.packet_type, b"PAR 2.0\0Main\0\0\0\0");
        assert_eq!(hdr.body_length(), 0);
    }

    #[test]
    fn reject_bad_magic() {
        let mut buf = make_header(64, b"PAR 2.0\0Main\0\0\0\0");
        buf[0] = b'X';
        assert!(PacketHeader::from_bytes(&buf).is_none());
    }

    #[test]
    fn reject_length_too_small() {
        let buf = make_header(32, b"PAR 2.0\0Main\0\0\0\0");
        assert!(PacketHeader::from_bytes(&buf).is_none());
    }

    #[test]
    fn reject_length_not_aligned() {
        let buf = make_header(65, b"PAR 2.0\0Main\0\0\0\0");
        assert!(PacketHeader::from_bytes(&buf).is_none());
    }

    #[test]
    fn body_length_calculation() {
        let buf = make_header(128, b"PAR 2.0\0Main\0\0\0\0");
        let hdr = PacketHeader::from_bytes(&buf).unwrap();
        assert_eq!(hdr.body_length(), 64);
    }
}
