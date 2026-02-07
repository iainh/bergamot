use std::sync::OnceLock;

const PRIMITIVE_POLY: u32 = 0x1100B;
const LIMIT: usize = 65535;

struct Tables {
    log: Box<[u16; 65536]>,
    exp: Box<[u16; 131070]>,
}

static TABLES: OnceLock<Tables> = OnceLock::new();

fn tables() -> &'static Tables {
    TABLES.get_or_init(|| {
        let mut log = Box::new([0u16; 65536]);
        let mut exp = Box::new([0u16; 131070]);

        let mut b: u32 = 1;
        for l in 0..LIMIT {
            log[b as usize] = l as u16;
            exp[l] = b as u16;

            b <<= 1;
            if b & 0x10000 != 0 {
                b ^= PRIMITIVE_POLY;
            }
        }

        log[0] = 0;
        for l in 0..LIMIT {
            exp[l + LIMIT] = exp[l];
        }

        Tables { log, exp }
    })
}

#[inline]
pub fn mul(a: u16, b: u16) -> u16 {
    if a == 0 || b == 0 {
        return 0;
    }
    let t = tables();
    t.exp[(t.log[a as usize] as usize) + (t.log[b as usize] as usize)]
}

#[inline]
pub fn div(a: u16, b: u16) -> u16 {
    assert!(b != 0, "division by zero in GF(2^16)");
    if a == 0 {
        return 0;
    }
    let t = tables();
    let log_a = t.log[a as usize] as usize;
    let log_b = t.log[b as usize] as usize;
    let log_result = if log_a >= log_b {
        log_a - log_b
    } else {
        log_a + LIMIT - log_b
    };
    t.exp[log_result]
}

#[inline]
pub fn inv(a: u16) -> u16 {
    assert!(a != 0, "inverse of zero in GF(2^16)");
    let t = tables();
    t.exp[LIMIT - t.log[a as usize] as usize]
}

#[inline]
pub fn pow(a: u16, n: u32) -> u16 {
    if n == 0 {
        return 1;
    }
    if a == 0 {
        return 0;
    }
    let t = tables();
    let log_a = t.log[a as usize] as usize;
    let result = (log_a * n as usize) % LIMIT;
    t.exp[result]
}

pub struct MulTable {
    lo: [u16; 256],
    hi: [u16; 256],
}

impl MulTable {
    pub fn new(coeff: u16) -> Self {
        let mut lo = [0u16; 256];
        let mut hi = [0u16; 256];
        if coeff != 0 {
            for i in 1..256 {
                lo[i] = mul(coeff, i as u16);
                hi[i] = mul(coeff, (i as u16) << 8);
            }
        }
        MulTable { lo, hi }
    }

    #[inline]
    pub fn mul_u16(&self, w: u16) -> u16 {
        self.lo[(w & 0xFF) as usize] ^ self.hi[(w >> 8) as usize]
    }
}

pub fn muladd(dst: &mut [u8], src: &[u8], coeff: u16) {
    if coeff == 0 {
        return;
    }
    let table = MulTable::new(coeff);
    muladd_with_table(dst, src, &table);
}

pub fn muladd_with_table(dst: &mut [u8], src: &[u8], table: &MulTable) {
    let len = dst.len().min(src.len());
    let words = len / 2;

    let dst_ptr = dst.as_mut_ptr() as *mut u16;
    let src_ptr = src.as_ptr() as *const u16;

    for i in 0..words {
        unsafe {
            let s = src_ptr.add(i).read_unaligned();
            let d = dst_ptr.add(i).read_unaligned();
            dst_ptr
                .add(i)
                .write_unaligned(d ^ table.mul_u16(u16::from_le(s)).to_le());
        }
    }

    if len % 2 == 1 {
        let last = len - 1;
        let s = src[last] as u16;
        let product = table.lo[s as usize];
        dst[last] ^= product as u8;
    }
}

pub fn mul_slice_inplace(dst: &mut [u8], coeff: u16) {
    if coeff == 0 {
        dst.fill(0);
        return;
    }
    if coeff == 1 {
        return;
    }
    let table = MulTable::new(coeff);
    let words = dst.len() / 2;
    let dst_ptr = dst.as_mut_ptr() as *mut u16;

    for i in 0..words {
        unsafe {
            let d = dst_ptr.add(i).read_unaligned();
            dst_ptr
                .add(i)
                .write_unaligned(table.mul_u16(u16::from_le(d)).to_le());
        }
    }

    if dst.len() % 2 == 1 {
        let last = dst.len() - 1;
        let d = dst[last] as u16;
        dst[last] = table.lo[d as usize] as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_table_starts_at_one() {
        let t = tables();
        assert_eq!(t.exp[0], 1);
    }

    #[test]
    fn log_exp_roundtrip() {
        let t = tables();
        for a in 1..=65535u16 {
            assert_eq!(t.exp[t.log[a as usize] as usize], a);
        }
    }

    #[test]
    fn mul_identity() {
        for a in [0u16, 1, 2, 255, 1000, 65535] {
            assert_eq!(mul(a, 1), a);
            assert_eq!(mul(1, a), a);
        }
    }

    #[test]
    fn mul_zero() {
        for a in [0u16, 1, 2, 65535] {
            assert_eq!(mul(a, 0), 0);
            assert_eq!(mul(0, a), 0);
        }
    }

    #[test]
    fn mul_commutative() {
        for &a in &[2u16, 3, 100, 1000, 50000] {
            for &b in &[5u16, 7, 200, 2000, 60000] {
                assert_eq!(mul(a, b), mul(b, a));
            }
        }
    }

    #[test]
    fn mul_inv_identity() {
        for &a in &[1u16, 2, 3, 100, 1000, 50000, 65535] {
            assert_eq!(mul(a, inv(a)), 1);
        }
    }

    #[test]
    fn div_is_mul_by_inv() {
        for &a in &[1u16, 2, 100, 50000] {
            for &b in &[1u16, 3, 200, 65535] {
                assert_eq!(div(a, b), mul(a, inv(b)));
            }
        }
    }

    #[test]
    fn distributive_law() {
        for &a in &[2u16, 100, 50000] {
            for &b in &[3u16, 200, 60000] {
                for &c in &[5u16, 300, 40000] {
                    assert_eq!(mul(a, b ^ c), mul(a, b) ^ mul(a, c));
                }
            }
        }
    }

    #[test]
    fn pow_basics() {
        assert_eq!(pow(0, 0), 1);
        assert_eq!(pow(0, 5), 0);
        assert_eq!(pow(5, 0), 1);
        assert_eq!(pow(5, 1), 5);
        assert_eq!(
            pow(2, 16),
            mul(
                mul(mul(mul(2, 2), mul(2, 2)), mul(mul(2, 2), mul(2, 2))),
                mul(mul(mul(2, 2), mul(2, 2)), mul(mul(2, 2), mul(2, 2)))
            )
        );
    }

    #[test]
    fn primitive_element_order() {
        assert_eq!(pow(2, 65535), 1);
        assert_ne!(pow(2, 65535 / 3), 1);
        assert_ne!(pow(2, 65535 / 5), 1);
        assert_ne!(pow(2, 65535 / 17), 1);
        assert_ne!(pow(2, 65535 / 257), 1);
    }

    #[test]
    fn multable_matches_mul() {
        let coeff = 12345u16;
        let table = MulTable::new(coeff);
        for w in (0..=65535u16).step_by(257) {
            assert_eq!(table.mul_u16(w), mul(coeff, w));
        }
    }

    #[test]
    fn muladd_basic() {
        let src = vec![0x01, 0x00, 0x02, 0x00];
        let mut dst = vec![0x00; 4];
        let coeff = 3u16;

        muladd(&mut dst, &src, coeff);

        let expected_w0 = mul(coeff, 1u16).to_le_bytes();
        let expected_w1 = mul(coeff, 2u16).to_le_bytes();
        assert_eq!(&dst[0..2], &expected_w0);
        assert_eq!(&dst[2..4], &expected_w1);
    }

    #[test]
    fn muladd_accumulates() {
        let src = vec![0x01, 0x00, 0x02, 0x00];
        let mut dst = vec![0xFF, 0xFF, 0xFF, 0xFF];
        let coeff = 5u16;

        muladd(&mut dst, &src, coeff);

        let w0 = u16::from_le_bytes([dst[0], dst[1]]);
        let w1 = u16::from_le_bytes([dst[2], dst[3]]);
        assert_eq!(w0, 0xFFFF ^ mul(coeff, 1));
        assert_eq!(w1, 0xFFFF ^ mul(coeff, 2));
    }

    #[test]
    fn muladd_zero_coeff_is_noop() {
        let src = vec![0x42; 8];
        let mut dst = vec![0x00; 8];
        muladd(&mut dst, &src, 0);
        assert_eq!(dst, vec![0x00; 8]);
    }

    #[test]
    fn mul_slice_inplace_basic() {
        let coeff = 7u16;
        let mut data = vec![0x03, 0x00, 0x05, 0x00];
        let original = data.clone();

        mul_slice_inplace(&mut data, coeff);

        let w0 = u16::from_le_bytes([data[0], data[1]]);
        let w1 = u16::from_le_bytes([data[2], data[3]]);
        let o0 = u16::from_le_bytes([original[0], original[1]]);
        let o1 = u16::from_le_bytes([original[2], original[3]]);
        assert_eq!(w0, mul(coeff, o0));
        assert_eq!(w1, mul(coeff, o1));
    }

    #[test]
    fn mul_slice_inplace_zero_clears() {
        let mut data = vec![0x42; 8];
        mul_slice_inplace(&mut data, 0);
        assert_eq!(data, vec![0x00; 8]);
    }

    #[test]
    fn mul_slice_inplace_one_is_noop() {
        let original = vec![0x42, 0x13, 0x37, 0xFF];
        let mut data = original.clone();
        mul_slice_inplace(&mut data, 1);
        assert_eq!(data, original);
    }
}
