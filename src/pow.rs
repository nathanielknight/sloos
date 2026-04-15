//! Hashcash-style proof-of-work.
//!
//! Given a nonce and a pow (both hex strings) and a difficulty, the PoW is
//! valid iff `sha256(nonce_bytes || pow_bytes)` has at least `difficulty`
//! leading zero bits.

use sha2::{Digest, Sha256};

/// Returns the number of leading zero bits in `bytes`.
pub fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    for b in bytes {
        if *b == 0 {
            count += 8;
        } else {
            count += b.leading_zeros();
            break;
        }
    }
    count
}

/// Compute the PoW hash of `concat(nonce, pow)`.
pub fn pow_hash(nonce: &[u8], pow: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(pow);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Verify a PoW. `nonce_hex` and `pow_hex` are decoded with hex before hashing.
/// Returns `Ok(true)` if valid, `Ok(false)` if invalid, `Err(_)` if the hex is
/// malformed.
pub fn verify(nonce_hex: &str, pow_hex: &str, difficulty: u32) -> Result<bool, hex::FromHexError> {
    let nonce = hex::decode(nonce_hex)?;
    let pow = hex::decode(pow_hex)?;
    let hash = pow_hash(&nonce, &pow);
    Ok(leading_zero_bits(&hash) >= difficulty)
}

/// Find a PoW for the given nonce at the given difficulty. Used in tests.
#[cfg(test)]
pub fn solve(nonce: &[u8], difficulty: u32) -> Vec<u8> {
    let mut counter: u64 = 0;
    loop {
        let pow = counter.to_be_bytes();
        let hash = pow_hash(nonce, &pow);
        if leading_zero_bits(&hash) >= difficulty {
            return pow.to_vec();
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn leading_zero_bits_all_zeros() {
        assert_eq!(leading_zero_bits(&[0, 0, 0, 0]), 32);
    }

    #[test]
    fn leading_zero_bits_all_ones() {
        assert_eq!(leading_zero_bits(&[0xFF, 0xFF]), 0);
    }

    #[test]
    fn leading_zero_bits_partial() {
        // 0x00 = 8 zeros, 0x0F = 4 leading zeros => 12
        assert_eq!(leading_zero_bits(&[0x00, 0x0F]), 12);
        // 0x01 = 7 leading zeros
        assert_eq!(leading_zero_bits(&[0x01]), 7);
        // 0x80 = 0 leading zeros (topmost bit set)
        assert_eq!(leading_zero_bits(&[0x80]), 0);
    }

    #[test]
    fn verify_zero_difficulty_always_ok() {
        let ok = verify("deadbeef", "cafebabe", 0).unwrap();
        assert!(ok);
    }

    #[test]
    fn verify_rejects_bad_pow() {
        // with reasonable difficulty arbitrary pow should fail
        let ok = verify("deadbeef", "00", 24).unwrap();
        assert!(!ok);
    }

    #[test]
    fn smoketest_verify_verify() {
        let nonce = b"some-nonce-bytes";
        let difficulty = 8;
        let pow = solve(nonce, difficulty);
        let nonce_hex = hex::encode(nonce);
        let pow_hex = hex::encode(&pow);
        assert!(verify(&nonce_hex, &pow_hex, difficulty).unwrap());
    }

    #[test]
    fn verify_rejects_malformed_hex() {
        assert!(verify("zz", "00", 0).is_err());
        assert!(verify("00", "zz", 0).is_err());
    }

    proptest! {
        // Property: for any nonce bytes, solving at difficulty d yields a pow
        // that verifies at difficulty d.
        #[test]
        fn solved_pow_verifies(nonce in proptest::collection::vec(any::<u8>(), 0..32), difficulty in 0u32..12) {
            let pow = solve(&nonce, difficulty);
            let hash = pow_hash(&nonce, &pow);
            prop_assert!(leading_zero_bits(&hash) >= difficulty);
        }

        // Property: verify (which takes hex strings) agrees with computing
        // the hash directly from raw bytes.
        #[test]
        fn verify_agrees_with_direct_hash(
            nonce in proptest::collection::vec(any::<u8>(), 0..32),
            pow in proptest::collection::vec(any::<u8>(), 0..32),
            difficulty in 0u32..32,
        ) {
            let hash = pow_hash(&nonce, &pow);
            let expected = leading_zero_bits(&hash) >= difficulty;
            let got = verify(&hex::encode(&nonce), &hex::encode(&pow), difficulty).unwrap();
            prop_assert_eq!(expected, got);
        }

        // Property: leading_zero_bits never exceeds bit length.
        #[test]
        fn leading_zero_bits_bounded(bytes in proptest::collection::vec(any::<u8>(), 0..32)) {
            let n = leading_zero_bits(&bytes) as usize;
            prop_assert!(n <= bytes.len() * 8);
        }
    }
}
