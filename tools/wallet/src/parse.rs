//! Parsing helpers for CLI argument → native types.
//!
//! - `parse_trs_to_wei`: decimal TRS → U256 (18 decimals). Used for all TRS transfers/staking.
//! - `parse_decimal_to_fixed_point`: decimal → FixedPoint (8 decimals, i128).
//!   Used for trading prices/quantities and market parameters.
//! - `parse_address`: hex string → 20-byte EVM address.
//! - `parse_pubkey`: 64 hex chars → 32-byte `PublicKey`.

use alloy_primitives::{Address, U256};
use torus_types::{FixedPoint, PublicKey};

pub(crate) fn parse_trs_to_wei(trs: &str) -> Result<U256, String> {
    let parts: Vec<&str> = trs.split('.').collect();
    match parts.len() {
        1 => {
            let whole: u128 = parts[0]
                .parse()
                .map_err(|e| format!("invalid amount: {e}"))?;
            Ok(U256::from(whole) * U256::from(1_000_000_000_000_000_000u64))
        }
        2 => {
            let whole: u128 = parts[0]
                .parse()
                .map_err(|e| format!("invalid amount: {e}"))?;
            let frac_str = parts[1];
            if frac_str.len() > 18 {
                return Err("too many decimal places (max 18)".into());
            }
            let padded = format!("{frac_str:0<18}");
            let frac: u128 = padded
                .parse()
                .map_err(|e| format!("invalid fraction: {e}"))?;
            Ok(U256::from(whole) * U256::from(1_000_000_000_000_000_000u64) + U256::from(frac))
        }
        _ => Err("invalid amount format, expected '1.5' or '1'".into()),
    }
}

pub(crate) fn parse_address(s: &str) -> Result<Address, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 40 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid Ethereum address".into());
    }
    let bytes = hex::decode(s).map_err(|e| format!("hex: {e}"))?;
    Ok(Address::from_slice(&bytes))
}

/// Parse decimal string to `FixedPoint` (8 decimal places, i128 scaled by 10^8).
///
/// Accepts `"1.5"`, `".5"` (no leading zero), `"100"`, `"-1.5"`, `"0.00000001"`.
/// Up to 8 fractional digits.
/// Rejects empty strings, non-numeric input, and more than 8 decimal places.
pub(crate) fn parse_decimal_to_fixed_point(s: &str) -> Result<FixedPoint, String> {
    if s.is_empty() {
        return Err("empty decimal string".into());
    }
    let (sign, body) = match s.strip_prefix('-') {
        Some(rest) => (-1i128, rest),
        None => (1i128, s),
    };
    if body.is_empty() {
        return Err("invalid decimal: missing digits".into());
    }
    let parts: Vec<&str> = body.split('.').collect();
    let (whole_str, frac_str) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err("invalid decimal: multiple '.'".into()),
    };
    if whole_str.is_empty() && frac_str.is_empty() {
        return Err("invalid decimal: missing digits".into());
    }
    if frac_str.len() > FixedPoint::DECIMALS as usize {
        return Err(format!(
            "too many decimal places (max {})",
            FixedPoint::DECIMALS
        ));
    }
    let whole: i128 = if whole_str.is_empty() {
        0
    } else {
        whole_str
            .parse()
            .map_err(|e| format!("invalid whole part: {e}"))?
    };
    let frac: i128 = if frac_str.is_empty() {
        0
    } else {
        let padded = format!("{frac_str:0<8}");
        padded
            .parse()
            .map_err(|e| format!("invalid fraction: {e}"))?
    };
    let raw = whole
        .checked_mul(FixedPoint::SCALE)
        .and_then(|w| w.checked_add(frac))
        .ok_or("decimal overflow")?;
    Ok(FixedPoint::from_raw(sign * raw))
}

pub(crate) fn parse_pubkey(s: &str) -> Result<PublicKey, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid pubkey: expected 64 hex characters".into());
    }
    let bytes = hex::decode(s).map_err(|e| format!("pubkey hex: {e}"))?;
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(PublicKey(arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trs_to_wei() {
        assert_eq!(
            parse_trs_to_wei("1").unwrap(),
            U256::from(1_000_000_000_000_000_000u64)
        );
        assert_eq!(
            parse_trs_to_wei("0.5").unwrap(),
            U256::from(500_000_000_000_000_000u64)
        );
        assert_eq!(
            parse_trs_to_wei("10").unwrap(),
            U256::from(10_000_000_000_000_000_000u128)
        );
        assert_eq!(
            parse_trs_to_wei("1.5").unwrap(),
            U256::from(1_500_000_000_000_000_000u64)
        );
        assert_eq!(
            parse_trs_to_wei("0.000001").unwrap(),
            U256::from(1_000_000_000_000u64)
        );
    }

    #[test]
    fn test_parse_address() {
        let addr = parse_address("0x0000000000000000000000000000000000000001").unwrap();
        let expected = {
            let mut b = [0u8; 20];
            b[19] = 1;
            Address::from_slice(&b)
        };
        assert_eq!(addr, expected);
        assert!(parse_address("invalid").is_err());
        assert!(parse_address("0x123").is_err());
    }

    #[test]
    fn test_parse_decimal_to_fixed_point() {
        assert_eq!(
            parse_decimal_to_fixed_point("1.5").unwrap().raw(),
            150_000_000
        );
        assert_eq!(
            parse_decimal_to_fixed_point("100").unwrap().raw(),
            10_000_000_000
        );
        assert_eq!(parse_decimal_to_fixed_point("0.00000001").unwrap().raw(), 1);
        assert_eq!(
            parse_decimal_to_fixed_point("0.001").unwrap().raw(),
            100_000
        );
        assert_eq!(
            parse_decimal_to_fixed_point("-1.5").unwrap().raw(),
            -150_000_000
        );
        assert_eq!(parse_decimal_to_fixed_point("0").unwrap().raw(), 0);
        assert_eq!(
            parse_decimal_to_fixed_point(".5").unwrap().raw(),
            50_000_000
        );
        // 9 decimals — too many
        assert!(parse_decimal_to_fixed_point("1.123456789").is_err());
        assert!(parse_decimal_to_fixed_point("").is_err());
        assert!(parse_decimal_to_fixed_point("abc").is_err());
        assert!(parse_decimal_to_fixed_point("-").is_err());
        assert!(parse_decimal_to_fixed_point("1.2.3").is_err());
    }

    #[test]
    fn test_parse_pubkey() {
        let hex64 = format!("{}1", "0".repeat(63));
        assert_eq!(parse_pubkey(&hex64).unwrap().0[31], 1);
        assert_eq!(parse_pubkey(&format!("0x{hex64}")).unwrap().0[31], 1);
        assert!(parse_pubkey("0x123").is_err());
        assert!(parse_pubkey(&"z".repeat(64)).is_err());
        assert!(parse_pubkey("").is_err());
    }
}
