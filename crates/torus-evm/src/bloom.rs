use alloy_primitives::Bloom;
use sha3::{Digest, Keccak256};

const BLOOM_BYTE_LENGTH: usize = 256;

/// Compute the logs bloom filter for a set of EVM logs.
pub fn logs_bloom<'a, I>(logs: I) -> Bloom
where
    I: IntoIterator<Item = &'a alloy_primitives::Log>,
{
    let mut bloom = Bloom::ZERO;
    for log in logs {
        accrue_log(&mut bloom, log);
    }
    bloom
}

/// Add a single log entry to the bloom filter.
fn accrue_log(bloom: &mut Bloom, log: &alloy_primitives::Log) {
    bloom_insert(bloom, log.address.as_slice());
    for topic in log.data.topics() {
        bloom_insert(bloom, topic.as_slice());
    }
}

/// Insert one item into the bloom using the Ethereum m3:2048 hash function.
///
/// Takes 3 pairs of bytes from keccak256(input), each pair selects one of
/// 2048 bit positions in the 256-byte (big-endian) bloom filter.
fn bloom_insert(bloom: &mut Bloom, input: &[u8]) {
    let hash = Keccak256::digest(input);
    for i in 0..3 {
        let bit = ((hash[2 * i] as usize) << 8 | hash[2 * i + 1] as usize) & 0x7FF;
        bloom.0[BLOOM_BYTE_LENGTH - 1 - bit / 8] |= 1 << (bit % 8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, bytes, Log};

    #[test]
    fn empty_logs_produce_zero_bloom() {
        let bloom = logs_bloom(std::iter::empty());
        assert_eq!(bloom, Bloom::ZERO);
    }

    #[test]
    fn single_log_sets_bits() {
        let log = Log::new_unchecked(
            address!("0x0000000000000000000000000000000000000001"),
            vec![b256!(
                "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
            )],
            bytes!(""),
        );
        let bloom = logs_bloom(std::iter::once(&log));
        assert_ne!(bloom, Bloom::ZERO);
    }

    #[test]
    fn bloom_accumulates() {
        let log1 = Log::new_unchecked(
            address!("0x0000000000000000000000000000000000000001"),
            vec![],
            bytes!(""),
        );
        let log2 = Log::new_unchecked(
            address!("0x0000000000000000000000000000000000000002"),
            vec![],
            bytes!(""),
        );
        let bloom_both = logs_bloom([&log1, &log2]);
        let bloom1 = logs_bloom(std::iter::once(&log1));
        let bloom2 = logs_bloom(std::iter::once(&log2));

        // Combined bloom must contain bits from both individual blooms.
        for i in 0..256 {
            assert_eq!(bloom_both.0[i] & bloom1.0[i], bloom1.0[i]);
            assert_eq!(bloom_both.0[i] & bloom2.0[i], bloom2.0[i]);
        }
    }
}
