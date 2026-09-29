//! Stable FNV-1a 64-bit checksums for migration files.

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 16-character lowercase hex FNV-1a 64-bit digest of `bytes`.
pub fn fnv1a_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a64(bytes))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_offset_basis() {
        assert_eq!(fnv1a_hex(b""), "cbf29ce484222325");
    }

    #[test]
    fn known_vector() {
        assert_eq!(fnv1a_hex(b"a"), "af63dc4c8601ec8c");
    }
}
