//! CRC-32/ISO-HDLC, the checksum GPT puts on its header and its entry array.
//! Both are what separates a table from bytes that merely look like one.

const POLY: u32 = 0xEDB8_8320;

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check value the standard names for this polynomial.
    #[test]
    fn matches_the_published_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn an_empty_input_checksums_to_zero() {
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn a_single_flipped_bit_changes_the_checksum() {
        let mut bytes = vec![0u8; 512];
        let clean = crc32(&bytes);
        bytes[300] ^= 0x01;
        assert_ne!(crc32(&bytes), clean);
    }
}
