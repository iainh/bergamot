use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::Par2RepairError;
use crate::galois;
use crate::model::{FileVerifyStatus, RecoverySet, VerifyResult};

const LIMIT: usize = 65535;

#[derive(Debug, Clone)]
pub struct RepairReport {
    pub repaired_slices: usize,
    pub repaired_files: Vec<String>,
}

struct GlobalSlice {
    file_idx: usize,
    local_slice_idx: usize,
    file_offset: u64,
    write_len: usize,
}

fn compute_base(index: usize) -> u16 {
    let mut logbase = 0u32;
    for _ in 0..=index {
        while gcd(LIMIT as u32, logbase) != 1 {
            logbase += 1;
        }
        if logbase < LIMIT as u32 {
            logbase += 1;
        }
    }
    galois::pow(2, logbase - 1)
}

fn gcd(a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

pub fn repair_recovery_set(
    rs: &RecoverySet,
    verify: &VerifyResult,
    working_dir: &Path,
) -> Result<RepairReport, Par2RepairError> {
    if verify.all_ok() {
        return Ok(RepairReport {
            repaired_slices: 0,
            repaired_files: vec![],
        });
    }

    let slice_size = rs.slice_size as usize;

    let global_slices = build_global_slice_map(rs);

    let missing_globals = find_missing_global_indices(rs, verify, &global_slices);
    let k = missing_globals.len();

    if k == 0 {
        return Ok(RepairReport {
            repaired_slices: 0,
            repaired_files: vec![],
        });
    }

    if k > rs.recovery_slices.len() {
        return Err(Par2RepairError::NotEnoughRecoverySlices {
            needed: k,
            available: rs.recovery_slices.len(),
        });
    }

    let selected_recovery = &rs.recovery_slices[..k];

    let mut recovery_data: Vec<Vec<u8>> = Vec::with_capacity(k);
    for rslice in selected_recovery {
        let mut file = std::fs::File::open(&rslice.par2_path)?;
        file.seek(SeekFrom::Start(rslice.data_offset))?;
        let mut buf = vec![0u8; slice_size];
        let read_len = rslice.data_len.min(slice_size);
        file.read_exact(&mut buf[..read_len])?;
        recovery_data.push(buf);
    }

    let exponents: Vec<u32> = selected_recovery.iter().map(|r| r.exponent).collect();

    let total_source_slices = global_slices.len();
    let bases: Vec<u16> = (0..total_source_slices).map(compute_base).collect();
    let missing_set: std::collections::HashMap<usize, usize> = missing_globals
        .iter()
        .enumerate()
        .map(|(col, &gi)| (gi, col))
        .collect();

    let mut matrix = vec![0u16; k * k];

    let mut slice_buf = vec![0u8; slice_size];
    let mut global_j = 0usize;

    for (file_idx, entry) in rs.files.iter().enumerate() {
        let slice_count = if rs.slice_size > 0 {
            entry.length.div_ceil(rs.slice_size) as usize
        } else {
            0
        };
        if slice_count == 0 {
            continue;
        }

        let file_path = working_dir.join(&entry.filename);
        let has_present_slices = (0..slice_count)
            .any(|local| !missing_set.contains_key(&(global_j + local)));

        let mut file = if has_present_slices && file_path.is_file() {
            Some(std::fs::File::open(&file_path)?)
        } else {
            None
        };

        for local in 0..slice_count {
            let j = global_j + local;
            if let Some(&col) = missing_set.get(&j) {
                for (i, &exp) in exponents.iter().enumerate() {
                    matrix[i * k + col] = galois::pow(bases[j], exp);
                }
            } else {
                let gs = &global_slices[j];
                let f = file.as_mut().unwrap();
                f.seek(SeekFrom::Start(gs.file_offset))?;
                f.read_exact(&mut slice_buf[..gs.write_len])?;
                slice_buf[gs.write_len..].fill(0);

                for (i, &exp) in exponents.iter().enumerate() {
                    let coeff = galois::pow(bases[j], exp);
                    galois::muladd(&mut recovery_data[i], &slice_buf, coeff);
                }
            }
            let _ = file_idx;
        }

        global_j += slice_count;
    }

    gauss_eliminate(&mut matrix, &mut recovery_data, k, slice_size)?;

    let mut repaired_files = std::collections::HashSet::new();
    for (col, &gi) in missing_globals.iter().enumerate() {
        let gs = &global_slices[gi];
        let entry = &rs.files[gs.file_idx];

        let file_path = working_dir.join(&entry.filename);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&file_path)?;
        file.set_len(entry.length)?;
        file.seek(SeekFrom::Start(gs.file_offset))?;
        file.write_all(&recovery_data[col][..gs.write_len])?;

        repaired_files.insert(entry.filename.clone());
    }

    Ok(RepairReport {
        repaired_slices: k,
        repaired_files: repaired_files.into_iter().collect(),
    })
}

fn build_global_slice_map(rs: &RecoverySet) -> Vec<GlobalSlice> {
    let mut slices = Vec::new();
    for (file_idx, entry) in rs.files.iter().enumerate() {
        let slice_count = if rs.slice_size > 0 {
            entry.length.div_ceil(rs.slice_size) as usize
        } else {
            0
        };
        for local in 0..slice_count {
            let file_offset = local as u64 * rs.slice_size;
            let remaining = entry.length - file_offset;
            let write_len = remaining.min(rs.slice_size) as usize;
            slices.push(GlobalSlice {
                file_idx,
                local_slice_idx: local,
                file_offset,
                write_len,
            });
        }
    }
    slices
}

fn find_missing_global_indices(
    rs: &RecoverySet,
    verify: &VerifyResult,
    global_slices: &[GlobalSlice],
) -> Vec<usize> {
    let mut missing = Vec::new();

    for (gi, gs) in global_slices.iter().enumerate() {
        let entry = &rs.files[gs.file_idx];
        let vf = verify.files.iter().find(|f| f.filename == entry.filename);
        let is_missing = match vf {
            Some(f) => match &f.status {
                FileVerifyStatus::Ok => false,
                FileVerifyStatus::Missing => true,
                FileVerifyStatus::Damaged { bad_slices } => {
                    bad_slices.contains(&(gs.local_slice_idx as u32))
                }
            },
            None => true,
        };
        if is_missing {
            missing.push(gi);
        }
    }
    missing
}


fn gauss_eliminate(
    matrix: &mut [u16],
    rhs: &mut [Vec<u8>],
    k: usize,
    _slice_size: usize,
) -> Result<(), Par2RepairError> {
    for p in 0..k {
        let mut pivot_row = None;
        for r in p..k {
            if matrix[r * k + p] != 0 {
                pivot_row = Some(r);
                break;
            }
        }
        let pivot_row = pivot_row.ok_or(Par2RepairError::SingularMatrix)?;

        if pivot_row != p {
            for col in 0..k {
                matrix.swap(p * k + col, pivot_row * k + col);
            }
            rhs.swap(p, pivot_row);
        }

        let pivot_val = matrix[p * k + p];
        let pivot_inv = galois::inv(pivot_val);

        if pivot_inv != 1 {
            let inv_table = galois::MulTable::new(pivot_inv);
            for col in p..k {
                matrix[p * k + col] = inv_table.mul_u16(matrix[p * k + col]);
            }
            galois::mul_slice_inplace(&mut rhs[p], pivot_inv);
        }

        for r in 0..k {
            if r == p {
                continue;
            }
            let factor = matrix[r * k + p];
            if factor == 0 {
                continue;
            }

            let factor_table = galois::MulTable::new(factor);
            for col in p..k {
                let val = factor_table.mul_u16(matrix[p * k + col]);
                matrix[r * k + col] ^= val;
            }

            let (rhs_r, rhs_p) = if r < p {
                let (left, right) = rhs.split_at_mut(p);
                (&mut left[r], &right[0])
            } else {
                let (left, right) = rhs.split_at_mut(r);
                (&mut right[0], &left[p])
            };
            galois::muladd_with_table(rhs_r, rhs_p, &factor_table);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::galois;

    #[test]
    fn gcd_basic() {
        assert_eq!(gcd(65535, 1), 1);
        assert_eq!(gcd(65535, 3), 3);
        assert_eq!(gcd(65535, 7), 1);
        assert_eq!(gcd(12, 8), 4);
    }

    #[test]
    fn compute_base_values_are_distinct() {
        let bases: Vec<u16> = (0..100).map(compute_base).collect();
        let unique: std::collections::HashSet<u16> = bases.iter().copied().collect();
        assert_eq!(bases.len(), unique.len(), "bases must be distinct");
    }

    #[test]
    fn compute_base_values_are_nonzero() {
        for i in 0..100 {
            assert_ne!(compute_base(i), 0, "base {i} must be nonzero");
        }
    }

    #[test]
    fn gauss_eliminate_identity() {
        let mut matrix = vec![1, 0, 0, 1];
        let mut rhs = vec![vec![0x42, 0x00], vec![0x13, 0x00]];
        gauss_eliminate(&mut matrix, &mut rhs, 2, 2).unwrap();
        assert_eq!(rhs[0], vec![0x42, 0x00]);
        assert_eq!(rhs[1], vec![0x13, 0x00]);
    }

    #[test]
    fn gauss_eliminate_swap_rows() {
        let mut matrix = vec![0, 1, 1, 0];
        let mut rhs = vec![vec![0xAA, 0x00], vec![0xBB, 0x00]];
        gauss_eliminate(&mut matrix, &mut rhs, 2, 2).unwrap();
        assert_eq!(rhs[0], vec![0xBB, 0x00]);
        assert_eq!(rhs[1], vec![0xAA, 0x00]);
    }

    #[test]
    fn gauss_eliminate_singular_fails() {
        let mut matrix = vec![1, 1, 1, 1];
        let mut rhs = vec![vec![0x00; 2], vec![0x00; 2]];
        let result = gauss_eliminate(&mut matrix, &mut rhs, 2, 2);
        assert!(result.is_err());
    }

    #[test]
    fn synthetic_rs_encode_decode() {
        let n = 6;
        let k = 2;
        let slice_size = 128;

        let source_slices: Vec<Vec<u8>> = (0..n)
            .map(|i| {
                (0..slice_size)
                    .map(|b| ((b * (i + 1)) % 251) as u8)
                    .collect()
            })
            .collect();

        let exponents = [0u32, 1];
        let generators: Vec<u16> = exponents.iter().map(|&e| galois::pow(2, e)).collect();

        let bases: Vec<u16> = (0..n).map(compute_base).collect();

        let mut recovery: Vec<Vec<u8>> = vec![vec![0u8; slice_size]; k];
        for (i, _) in generators.iter().enumerate() {
            for (j, src) in source_slices.iter().enumerate() {
                let factor = galois::pow(bases[j], exponents[i]);
                galois::muladd(&mut recovery[i], src, factor);
            }
        }

        let missing = [1usize, 4];

        let mut rhs = recovery.clone();
        for (j, src) in source_slices.iter().enumerate() {
            if missing.contains(&j) {
                continue;
            }
            for (i, _) in exponents.iter().enumerate() {
                let factor = galois::pow(bases[j], exponents[i]);
                galois::muladd(&mut rhs[i], src, factor);
            }
        }

        let mut matrix = vec![0u16; k * k];
        for (col, &mj) in missing.iter().enumerate() {
            for (row, &exp) in exponents.iter().enumerate() {
                matrix[row * k + col] = galois::pow(bases[mj], exp);
            }
        }

        gauss_eliminate(&mut matrix, &mut rhs, k, slice_size).unwrap();

        for (col, &mj) in missing.iter().enumerate() {
            assert_eq!(
                rhs[col], source_slices[mj],
                "recovered slice {mj} doesn't match original"
            );
        }
    }

    #[test]
    fn synthetic_rs_vandermonde_3_missing() {
        let n = 8;
        let k = 3;
        let slice_size = 64;

        let source_slices: Vec<Vec<u8>> = (0..n)
            .map(|i| {
                (0..slice_size)
                    .map(|b| ((b * (i + 7)) % 199) as u8)
                    .collect()
            })
            .collect();

        let exponents = [0u32, 1, 2];
        let bases: Vec<u16> = (0..n).map(compute_base).collect();

        let recovery: Vec<Vec<u8>> = (0..k)
            .map(|i| {
                let mut buf = vec![0u8; slice_size];
                for j in 0..n {
                    let factor = galois::pow(bases[j], exponents[i]);
                    galois::muladd(&mut buf, &source_slices[j], factor);
                }
                buf
            })
            .collect();

        let missing = [0usize, 3, 6];

        let mut rhs = recovery.clone();
        let mut matrix = vec![0u16; k * k];
        let missing_set: std::collections::HashMap<usize, usize> = missing
            .iter()
            .enumerate()
            .map(|(col, &gi)| (gi, col))
            .collect();

        for j in 0..n {
            if let Some(&col) = missing_set.get(&j) {
                for (i, &exp) in exponents.iter().enumerate() {
                    matrix[i * k + col] = galois::pow(bases[j], exp);
                }
            } else {
                for (i, &exp) in exponents.iter().enumerate() {
                    let coeff = galois::pow(bases[j], exp);
                    galois::muladd(&mut rhs[i], &source_slices[j], coeff);
                }
            }
        }

        gauss_eliminate(&mut matrix, &mut rhs, k, slice_size).unwrap();

        for (col, &mj) in missing.iter().enumerate() {
            assert_eq!(
                rhs[col], source_slices[mj],
                "recovered slice {mj} doesn't match original"
            );
        }
    }
}
