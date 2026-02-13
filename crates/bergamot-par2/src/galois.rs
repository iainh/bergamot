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
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    nibble: NibbleTable,
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
struct NibbleTable {
    lo: [[u8; 16]; 4],
    hi: [[u8; 16]; 4],
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

        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        let nibble = {
            let mut tbl = NibbleTable {
                lo: [[0u8; 16]; 4],
                hi: [[0u8; 16]; 4],
            };
            if coeff != 0 {
                for p in 0..4 {
                    for n in 0..16u16 {
                        let w = n << (p * 4);
                        let prod = mul(coeff, w);
                        tbl.lo[p][n as usize] = (prod & 0xFF) as u8;
                        tbl.hi[p][n as usize] = (prod >> 8) as u8;
                    }
                }
            }
            tbl
        };

        MulTable {
            lo,
            hi,
            #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
            nibble,
        }
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

    #[cfg(target_arch = "aarch64")]
    {
        muladd_neon(dst, src, len, table);
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("ssse3") {
            unsafe { muladd_ssse3(dst, src, len, table) };
        } else {
            muladd_scalar(dst, src, len, table);
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    muladd_scalar(dst, src, len, table);
}

fn muladd_scalar(dst: &mut [u8], src: &[u8], len: usize, table: &MulTable) {
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

#[cfg(target_arch = "aarch64")]
fn muladd_neon(dst: &mut [u8], src: &[u8], len: usize, table: &MulTable) {
    use core::arch::aarch64::*;

    if len < 16 {
        muladd_scalar(dst, src, len, table);
        return;
    }

    unsafe {
        let mask_0f = vdupq_n_u8(0x0F);

        let t0_lo = vld1q_u8(table.nibble.lo[0].as_ptr());
        let t0_hi = vld1q_u8(table.nibble.hi[0].as_ptr());
        let t1_lo = vld1q_u8(table.nibble.lo[1].as_ptr());
        let t1_hi = vld1q_u8(table.nibble.hi[1].as_ptr());
        let t2_lo = vld1q_u8(table.nibble.lo[2].as_ptr());
        let t2_hi = vld1q_u8(table.nibble.hi[2].as_ptr());
        let t3_lo = vld1q_u8(table.nibble.lo[3].as_ptr());
        let t3_hi = vld1q_u8(table.nibble.hi[3].as_ptr());

        let chunks = len / 16;
        let mut offset = 0;

        for _ in 0..chunks {
            let s = vld1q_u8(src.as_ptr().add(offset));
            let d = vld1q_u8(dst.as_ptr().add(offset));

            let even_bytes = vuzp1q_u8(s, vdupq_n_u8(0));
            let odd_bytes = vuzp2q_u8(s, vdupq_n_u8(0));

            let n0 = vandq_u8(even_bytes, mask_0f);
            let n1 = vshrq_n_u8(even_bytes, 4);
            let n2 = vandq_u8(odd_bytes, mask_0f);
            let n3 = vshrq_n_u8(odd_bytes, 4);

            let r_lo = veorq_u8(
                veorq_u8(vqtbl1q_u8(t0_lo, n0), vqtbl1q_u8(t1_lo, n1)),
                veorq_u8(vqtbl1q_u8(t2_lo, n2), vqtbl1q_u8(t3_lo, n3)),
            );
            let r_hi = veorq_u8(
                veorq_u8(vqtbl1q_u8(t0_hi, n0), vqtbl1q_u8(t1_hi, n1)),
                veorq_u8(vqtbl1q_u8(t2_hi, n2), vqtbl1q_u8(t3_hi, n3)),
            );

            let product = vzip1q_u8(r_lo, r_hi);
            vst1q_u8(dst.as_mut_ptr().add(offset), veorq_u8(d, product));
            offset += 16;
        }
    }

    let tail_start = (len / 16) * 16;
    if tail_start < len {
        muladd_scalar(
            &mut dst[tail_start..],
            &src[tail_start..],
            len - tail_start,
            table,
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn muladd_ssse3(dst: &mut [u8], src: &[u8], len: usize, table: &MulTable) {
    use core::arch::x86_64::*;

    if len < 16 {
        muladd_scalar(dst, src, len, table);
        return;
    }

    let mask_0f = _mm_set1_epi8(0x0F);

    let t0_lo = _mm_loadu_si128(table.nibble.lo[0].as_ptr() as *const __m128i);
    let t0_hi = _mm_loadu_si128(table.nibble.hi[0].as_ptr() as *const __m128i);
    let t1_lo = _mm_loadu_si128(table.nibble.lo[1].as_ptr() as *const __m128i);
    let t1_hi = _mm_loadu_si128(table.nibble.hi[1].as_ptr() as *const __m128i);
    let t2_lo = _mm_loadu_si128(table.nibble.lo[2].as_ptr() as *const __m128i);
    let t2_hi = _mm_loadu_si128(table.nibble.hi[2].as_ptr() as *const __m128i);
    let t3_lo = _mm_loadu_si128(table.nibble.lo[3].as_ptr() as *const __m128i);
    let t3_hi = _mm_loadu_si128(table.nibble.hi[3].as_ptr() as *const __m128i);

    let deinterleave_even =
        _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1);
    let deinterleave_odd = _mm_setr_epi8(1, 3, 5, 7, 9, 11, 13, 15, -1, -1, -1, -1, -1, -1, -1, -1);

    let chunks = len / 16;
    let mut offset = 0;

    for _ in 0..chunks {
        let s = _mm_loadu_si128(src.as_ptr().add(offset) as *const __m128i);
        let d = _mm_loadu_si128(dst.as_ptr().add(offset) as *const __m128i);

        let even_bytes = _mm_shuffle_epi8(s, deinterleave_even);
        let odd_bytes = _mm_shuffle_epi8(s, deinterleave_odd);

        let n0 = _mm_and_si128(even_bytes, mask_0f);
        let n1 = _mm_and_si128(_mm_srli_epi16(even_bytes, 4), mask_0f);
        let n2 = _mm_and_si128(odd_bytes, mask_0f);
        let n3 = _mm_and_si128(_mm_srli_epi16(odd_bytes, 4), mask_0f);

        let r_lo = _mm_xor_si128(
            _mm_xor_si128(_mm_shuffle_epi8(t0_lo, n0), _mm_shuffle_epi8(t1_lo, n1)),
            _mm_xor_si128(_mm_shuffle_epi8(t2_lo, n2), _mm_shuffle_epi8(t3_lo, n3)),
        );
        let r_hi = _mm_xor_si128(
            _mm_xor_si128(_mm_shuffle_epi8(t0_hi, n0), _mm_shuffle_epi8(t1_hi, n1)),
            _mm_xor_si128(_mm_shuffle_epi8(t2_hi, n2), _mm_shuffle_epi8(t3_hi, n3)),
        );

        let product = _mm_unpacklo_epi8(r_lo, r_hi);
        _mm_storeu_si128(
            dst.as_mut_ptr().add(offset) as *mut __m128i,
            _mm_xor_si128(d, product),
        );
        offset += 16;
    }

    let tail_start = chunks * 16;
    if tail_start < len {
        muladd_scalar(
            &mut dst[tail_start..],
            &src[tail_start..],
            len - tail_start,
            table,
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn mul_slice_inplace_ssse3(dst: &mut [u8], table: &MulTable) {
    use core::arch::x86_64::*;

    let len = dst.len();
    if len < 16 {
        mul_slice_inplace_scalar(dst, table);
        return;
    }

    let mask_0f = _mm_set1_epi8(0x0F);

    let t0_lo = _mm_loadu_si128(table.nibble.lo[0].as_ptr() as *const __m128i);
    let t0_hi = _mm_loadu_si128(table.nibble.hi[0].as_ptr() as *const __m128i);
    let t1_lo = _mm_loadu_si128(table.nibble.lo[1].as_ptr() as *const __m128i);
    let t1_hi = _mm_loadu_si128(table.nibble.hi[1].as_ptr() as *const __m128i);
    let t2_lo = _mm_loadu_si128(table.nibble.lo[2].as_ptr() as *const __m128i);
    let t2_hi = _mm_loadu_si128(table.nibble.hi[2].as_ptr() as *const __m128i);
    let t3_lo = _mm_loadu_si128(table.nibble.lo[3].as_ptr() as *const __m128i);
    let t3_hi = _mm_loadu_si128(table.nibble.hi[3].as_ptr() as *const __m128i);

    let deinterleave_even =
        _mm_setr_epi8(0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1);
    let deinterleave_odd = _mm_setr_epi8(1, 3, 5, 7, 9, 11, 13, 15, -1, -1, -1, -1, -1, -1, -1, -1);

    let chunks = len / 16;
    let mut offset = 0;

    for _ in 0..chunks {
        let d = _mm_loadu_si128(dst.as_ptr().add(offset) as *const __m128i);

        let even_bytes = _mm_shuffle_epi8(d, deinterleave_even);
        let odd_bytes = _mm_shuffle_epi8(d, deinterleave_odd);

        let n0 = _mm_and_si128(even_bytes, mask_0f);
        let n1 = _mm_and_si128(_mm_srli_epi16(even_bytes, 4), mask_0f);
        let n2 = _mm_and_si128(odd_bytes, mask_0f);
        let n3 = _mm_and_si128(_mm_srli_epi16(odd_bytes, 4), mask_0f);

        let r_lo = _mm_xor_si128(
            _mm_xor_si128(_mm_shuffle_epi8(t0_lo, n0), _mm_shuffle_epi8(t1_lo, n1)),
            _mm_xor_si128(_mm_shuffle_epi8(t2_lo, n2), _mm_shuffle_epi8(t3_lo, n3)),
        );
        let r_hi = _mm_xor_si128(
            _mm_xor_si128(_mm_shuffle_epi8(t0_hi, n0), _mm_shuffle_epi8(t1_hi, n1)),
            _mm_xor_si128(_mm_shuffle_epi8(t2_hi, n2), _mm_shuffle_epi8(t3_hi, n3)),
        );

        let product = _mm_unpacklo_epi8(r_lo, r_hi);
        _mm_storeu_si128(dst.as_mut_ptr().add(offset) as *mut __m128i, product);
        offset += 16;
    }

    let tail_start = chunks * 16;
    if tail_start < len {
        mul_slice_inplace_scalar(&mut dst[tail_start..], table);
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

    #[cfg(target_arch = "aarch64")]
    {
        mul_slice_inplace_neon(dst, &table);
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("ssse3") {
            unsafe { mul_slice_inplace_ssse3(dst, &table) };
        } else {
            mul_slice_inplace_scalar(dst, &table);
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    mul_slice_inplace_scalar(dst, &table);
}

fn mul_slice_inplace_scalar(dst: &mut [u8], table: &MulTable) {
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

#[cfg(target_arch = "aarch64")]
fn mul_slice_inplace_neon(dst: &mut [u8], table: &MulTable) {
    use core::arch::aarch64::*;

    let len = dst.len();
    if len < 16 {
        mul_slice_inplace_scalar(dst, table);
        return;
    }

    unsafe {
        let mask_0f = vdupq_n_u8(0x0F);

        let t0_lo = vld1q_u8(table.nibble.lo[0].as_ptr());
        let t0_hi = vld1q_u8(table.nibble.hi[0].as_ptr());
        let t1_lo = vld1q_u8(table.nibble.lo[1].as_ptr());
        let t1_hi = vld1q_u8(table.nibble.hi[1].as_ptr());
        let t2_lo = vld1q_u8(table.nibble.lo[2].as_ptr());
        let t2_hi = vld1q_u8(table.nibble.hi[2].as_ptr());
        let t3_lo = vld1q_u8(table.nibble.lo[3].as_ptr());
        let t3_hi = vld1q_u8(table.nibble.hi[3].as_ptr());

        let chunks = len / 16;
        let mut offset = 0;

        for _ in 0..chunks {
            let d = vld1q_u8(dst.as_ptr().add(offset));

            let even_bytes = vuzp1q_u8(d, vdupq_n_u8(0));
            let odd_bytes = vuzp2q_u8(d, vdupq_n_u8(0));

            let n0 = vandq_u8(even_bytes, mask_0f);
            let n1 = vshrq_n_u8(even_bytes, 4);
            let n2 = vandq_u8(odd_bytes, mask_0f);
            let n3 = vshrq_n_u8(odd_bytes, 4);

            let r_lo = veorq_u8(
                veorq_u8(vqtbl1q_u8(t0_lo, n0), vqtbl1q_u8(t1_lo, n1)),
                veorq_u8(vqtbl1q_u8(t2_lo, n2), vqtbl1q_u8(t3_lo, n3)),
            );
            let r_hi = veorq_u8(
                veorq_u8(vqtbl1q_u8(t0_hi, n0), vqtbl1q_u8(t1_hi, n1)),
                veorq_u8(vqtbl1q_u8(t2_hi, n2), vqtbl1q_u8(t3_hi, n3)),
            );

            let product = vzip1q_u8(r_lo, r_hi);
            vst1q_u8(dst.as_mut_ptr().add(offset), product);
            offset += 16;
        }
    }

    let tail_start = (len / 16) * 16;
    if tail_start < len {
        mul_slice_inplace_scalar(&mut dst[tail_start..], table);
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
