//! Order-book storage mode: a CHAIN-WIDE genesis field (`consensus.book_mode`,
//! owner s131).
//!
//! Modes 1-3 change the state format (consensus-visible: the book CF rows
//! that feed the native root), so every node of a chain must use the same
//! mode. It comes from genesis, so a fleet sharing a genesis agrees by
//! construction; there is no activation height (the mode is fixed for the
//! chain's life; changing it needs a fresh genesis).
//!
//! The legacy node env `TORUS_BOOK_ROWS` may only repeat the genesis mode
//! (accepted with a deprecation warning); a value that disagrees, or is not a
//! mode, refuses to start ([`resolve_node_book_mode`]). The node-local
//! `__book_mode__` marker row (written at genesis and on the first save) must
//! name the same mode ([`check_book_mode_marker`]).
//!
//! The node sets the process-wide mode once at startup
//! ([`set_process_book_mode`]); everything that builds an execution context
//! outside the consensus executor (RPC reads, the checkpoint layer) reads it
//! through [`process_book_mode`].

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Consensus-visible book persistence mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BookMode {
    /// Whole-book borsh blob per market (8-byte key); the default (genesis
    /// field absent).
    #[default]
    Classic,
    /// C4 per-order rows in the root CF (mode 1).
    OrderRows,
    /// 3c level rows in the root CF + node-local order rows (mode 2).
    LevelAuthority,
    /// Mode 2's layout with the CHUNKED, depth-independent `level_hash`
    /// preimage (mode 3).
    LevelAuthorityChunked,
}

/// Every mode, in discriminant order.
pub const ALL_BOOK_MODES: [BookMode; 4] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];

impl BookMode {
    /// One-byte discriminant persisted in the `__book_mode__` marker row; also
    /// the integer form of the genesis field and the `TORUS_BOOK_ROWS` value.
    pub fn marker_byte(self) -> u8 {
        match self {
            BookMode::Classic => 0,
            BookMode::OrderRows => 1,
            BookMode::LevelAuthority => 2,
            BookMode::LevelAuthorityChunked => 3,
        }
    }

    /// Inverse of [`Self::marker_byte`].
    pub fn from_marker_byte(b: u8) -> Option<BookMode> {
        ALL_BOOK_MODES.get(b as usize).copied()
    }

    /// The genesis spelling (`consensus.book_mode`).
    pub fn name(self) -> &'static str {
        match self {
            BookMode::Classic => "classic",
            BookMode::OrderRows => "order_rows",
            BookMode::LevelAuthority => "level_authority",
            BookMode::LevelAuthorityChunked => "level_authority_chunked",
        }
    }

    /// Parse the genesis spelling (exact, lowercase).
    pub fn from_name(s: &str) -> Option<BookMode> {
        ALL_BOOK_MODES.into_iter().find(|m| m.name() == s)
    }

    /// Both level-authority variants share the row layout, loader, saver and
    /// boot verify; they differ only in the `level_hash` preimage.
    pub fn is_level_authority(self) -> bool {
        matches!(
            self,
            BookMode::LevelAuthority | BookMode::LevelAuthorityChunked
        )
    }

    /// True for Classic (the default; `ChainConfig` omits it when serialized).
    pub fn is_classic(&self) -> bool {
        matches!(self, BookMode::Classic)
    }

    /// True for the chunked-digest variant (mode 3).
    pub fn level_hash_chunked(self) -> bool {
        matches!(self, BookMode::LevelAuthorityChunked)
    }

    /// Human description for logs and errors.
    pub fn describe(self) -> &'static str {
        match self {
            BookMode::Classic => "classic (whole-book blobs, mode 0)",
            BookMode::OrderRows => "order rows (mode 1)",
            BookMode::LevelAuthority => "level authority (mode 2)",
            BookMode::LevelAuthorityChunked => "level authority, chunked level hash (mode 3)",
        }
    }
}

impl std::fmt::Display for BookMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for BookMode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

/// Genesis form: the name (`"level_authority_chunked"`) or its number (`3`).
/// Anything else is an error: a typo never becomes a silent Classic.
impl<'de> Deserialize<'de> for BookMode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = BookMode;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "a book mode: \"classic\", \"order_rows\", \"level_authority\", \
                     \"level_authority_chunked\" or 0-3",
                )
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<BookMode, E> {
                BookMode::from_name(v)
                    .ok_or_else(|| E::invalid_value(serde::de::Unexpected::Str(v), &self))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<BookMode, E> {
                u8::try_from(v)
                    .ok()
                    .and_then(BookMode::from_marker_byte)
                    .ok_or_else(|| E::invalid_value(serde::de::Unexpected::Unsigned(v), &self))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<BookMode, E> {
                u64::try_from(v)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Signed(v), &self))
                    .and_then(|u| self.visit_u64(u))
            }
        }
        d.deserialize_any(V)
    }
}

/// The legacy node env variable; may only repeat the genesis mode.
pub const BOOK_ROWS_ENV: &str = "TORUS_BOOK_ROWS";

/// Strict parse of a `TORUS_BOOK_ROWS` value: exactly `0`, `1`, `2` or `3`
/// after trimming (no sign, no leading zero). `Ok(None)` = unset or blank (the
/// genesis mode applies).
pub fn parse_book_rows_env(v: Option<&str>) -> Result<Option<BookMode>, String> {
    let Some(raw) = v else { return Ok(None) };
    let t = raw.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t {
        "0" => Ok(Some(BookMode::Classic)),
        "1" => Ok(Some(BookMode::OrderRows)),
        "2" => Ok(Some(BookMode::LevelAuthority)),
        "3" => Ok(Some(BookMode::LevelAuthorityChunked)),
        _ => Err(format!(
            "{BOOK_ROWS_ENV}={raw:?} is not a book mode (0, 1, 2 or 3)"
        )),
    }
}

/// Lenient legacy parse (the pre-genesis `parse_book_rows_mode`): trimmed
/// `1`/`2`/`3` select that mode, anything else (incl. unset) is Classic. Used
/// only by processes that never set a chain mode (tests, offline tools).
pub fn parse_book_rows_legacy(v: Option<&str>) -> BookMode {
    match v.map(str::trim) {
        Some("1") => BookMode::OrderRows,
        Some("2") => BookMode::LevelAuthority,
        Some("3") => BookMode::LevelAuthorityChunked,
        _ => BookMode::Classic,
    }
}

/// The node's book mode from the chain's (genesis) mode and the
/// `TORUS_BOOK_ROWS` value: unset/blank = the chain mode; equal = the chain
/// mode, `deprecated_env` set (the caller warns); different or not a mode =
/// `Err` (refuse to start, no silent override).
pub fn resolve_node_book_mode(
    chain: BookMode,
    env: Option<&str>,
) -> Result<ResolvedBookMode, String> {
    let fix = format!(
        "unset {BOOK_ROWS_ENV} (deprecated: the book mode comes from genesis consensus.book_mode, \
         \"{}\" on this chain, = {BOOK_ROWS_ENV}={})",
        chain.name(),
        chain.marker_byte()
    );
    match parse_book_rows_env(env) {
        Ok(None) => Ok(ResolvedBookMode {
            mode: chain,
            deprecated_env: false,
        }),
        Ok(Some(m)) if m == chain => Ok(ResolvedBookMode {
            mode: chain,
            deprecated_env: true,
        }),
        Ok(Some(m)) => Err(format!(
            "{BOOK_ROWS_ENV}={} selects {} but genesis consensus.book_mode is {}: the book mode is \
             chain-wide and fixed by genesis; {fix}",
            m.marker_byte(),
            m.describe(),
            chain.describe()
        )),
        Err(e) => Err(format!("{e}; {fix}")),
    }
}

/// [`resolve_node_book_mode`]'s result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedBookMode {
    pub mode: BookMode,
    /// `TORUS_BOOK_ROWS` was set (to the chain mode): warn that it is
    /// deprecated.
    pub deprecated_env: bool,
}

/// The `__book_mode__` marker (`None` = no row yet: a fresh DB) must name
/// `chain`; a mismatch, or a byte that is no mode, refuses to start.
pub fn check_book_mode_marker(marker: Option<u8>, chain: BookMode) -> Result<(), String> {
    match marker {
        None => Ok(()),
        Some(b) if b == chain.marker_byte() => Ok(()),
        Some(b) => Err(format!(
            "book-mode marker mismatch: the DB was written under {} but genesis \
             consensus.book_mode is {} (the mode is chain-wide and fixed by genesis; this data \
             dir belongs to another chain, or the genesis file changed after it was applied)",
            BookMode::from_marker_byte(b)
                .map(BookMode::describe)
                .unwrap_or("an unknown mode"),
            chain.describe()
        )),
    }
}

/// Where the process-wide mode came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookModeSource {
    /// Set by the node from its chain config ([`set_process_book_mode`]).
    Chain,
    /// First read before any chain mode was set: the legacy env parse (tests,
    /// offline tools; never the node, which sets the chain mode first).
    LegacyEnv,
}

static PROCESS_BOOK_MODE: std::sync::OnceLock<(BookMode, BookModeSource)> =
    std::sync::OnceLock::new();

/// Set this process's book mode (the node: the resolved genesis mode) once, at
/// startup, before any execution context exists. Fails if the process already
/// uses another mode (set earlier, or read before this call), so one process
/// can never mix two modes.
pub fn set_process_book_mode(mode: BookMode) -> Result<(), String> {
    match *PROCESS_BOOK_MODE.get_or_init(|| (mode, BookModeSource::Chain)) {
        (got, _) if got == mode => Ok(()),
        (got, src) => Err(format!(
            "process book mode is already {} ({src:?}); cannot set {}",
            got.describe(),
            mode.describe()
        )),
    }
}

/// This process's book mode: the one [`set_process_book_mode`] set, else (and
/// from then on) the legacy `TORUS_BOOK_ROWS` parse.
pub fn process_book_mode() -> BookMode {
    PROCESS_BOOK_MODE
        .get_or_init(|| {
            (
                parse_book_rows_legacy(std::env::var(BOOK_ROWS_ENV).ok().as_deref()),
                BookModeSource::LegacyEnv,
            )
        })
        .0
}

/// The chain mode the node set, if it did ([`BookModeSource::Chain`]).
pub fn chain_book_mode() -> Option<BookMode> {
    match PROCESS_BOOK_MODE.get() {
        Some((m, BookModeSource::Chain)) => Some(*m),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_bytes_names_and_numbers_round_trip() {
        for (i, m) in ALL_BOOK_MODES.into_iter().enumerate() {
            assert_eq!(m.marker_byte() as usize, i);
            assert_eq!(BookMode::from_marker_byte(i as u8), Some(m));
            assert_eq!(BookMode::from_name(m.name()), Some(m));
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(json, format!("\"{}\"", m.name()));
            assert_eq!(serde_json::from_str::<BookMode>(&json).unwrap(), m);
            assert_eq!(serde_json::from_str::<BookMode>(&i.to_string()).unwrap(), m);
        }
        assert_eq!(BookMode::from_marker_byte(4), None);
        assert_eq!(BookMode::default(), BookMode::Classic);
        for bad in [
            "4",
            "-1",
            "\"3\"",
            "\"Classic\"",
            "null",
            "true",
            "1.0",
            "\"\"",
        ] {
            assert!(serde_json::from_str::<BookMode>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn env_unset_or_blank_uses_the_chain_mode() {
        for chain in ALL_BOOK_MODES {
            for env in [None, Some(""), Some("  "), Some("\n")] {
                assert_eq!(
                    resolve_node_book_mode(chain, env),
                    Ok(ResolvedBookMode {
                        mode: chain,
                        deprecated_env: false
                    }),
                    "{chain:?} {env:?}"
                );
            }
        }
    }

    #[test]
    fn env_agreeing_is_accepted_and_flagged_deprecated() {
        for chain in ALL_BOOK_MODES {
            let b = chain.marker_byte().to_string();
            for env in [b.clone(), format!(" {b}\n")] {
                assert_eq!(
                    resolve_node_book_mode(chain, Some(&env)),
                    Ok(ResolvedBookMode {
                        mode: chain,
                        deprecated_env: true
                    }),
                    "{chain:?} {env:?}"
                );
            }
        }
    }

    #[test]
    fn env_disagreeing_or_garbage_refuses() {
        for chain in ALL_BOOK_MODES {
            for other in ALL_BOOK_MODES.into_iter().filter(|m| *m != chain) {
                let err = resolve_node_book_mode(chain, Some(&other.marker_byte().to_string()))
                    .unwrap_err();
                assert!(
                    err.contains("TORUS_BOOK_ROWS") && err.contains("consensus.book_mode"),
                    "{err}"
                );
                assert!(
                    err.contains(chain.describe()) && err.contains(other.describe()),
                    "{err}"
                );
            }
            for garbage in ["4", "x", "on", "-1", "03", "+3", "3 3", "classic"] {
                let err = resolve_node_book_mode(chain, Some(garbage)).unwrap_err();
                assert!(
                    err.contains("is not a book mode") && err.contains("consensus.book_mode"),
                    "{err}"
                );
            }
        }
    }

    #[test]
    fn marker_must_match_the_chain_mode() {
        for chain in ALL_BOOK_MODES {
            assert_eq!(check_book_mode_marker(None, chain), Ok(()));
            assert_eq!(
                check_book_mode_marker(Some(chain.marker_byte()), chain),
                Ok(())
            );
            for other in ALL_BOOK_MODES.into_iter().filter(|m| *m != chain) {
                let err = check_book_mode_marker(Some(other.marker_byte()), chain).unwrap_err();
                assert!(err.contains("book-mode marker mismatch"), "{err}");
                assert!(
                    err.contains(other.describe()) && err.contains(chain.describe()),
                    "{err}"
                );
            }
            assert!(check_book_mode_marker(Some(9), chain)
                .unwrap_err()
                .contains("unknown mode"));
        }
    }

    #[test]
    fn legacy_parse_is_the_old_lenient_one() {
        assert_eq!(parse_book_rows_legacy(None), BookMode::Classic);
        assert_eq!(parse_book_rows_legacy(Some("x")), BookMode::Classic);
        assert_eq!(
            parse_book_rows_legacy(Some(" 3\n")),
            BookMode::LevelAuthorityChunked
        );
        assert_eq!(parse_book_rows_legacy(Some("2")), BookMode::LevelAuthority);
        assert_eq!(parse_book_rows_legacy(Some("1")), BookMode::OrderRows);
        assert_eq!(parse_book_rows_legacy(Some("0")), BookMode::Classic);
    }

    /// Set once; a conflicting set fails. Only Classic is set here: tests share
    /// the process (and the env is unset in the test runner).
    #[test]
    fn process_mode_is_set_once() {
        assert_eq!(set_process_book_mode(BookMode::Classic), Ok(()));
        assert_eq!(set_process_book_mode(BookMode::Classic), Ok(()));
        assert!(set_process_book_mode(BookMode::LevelAuthorityChunked).is_err());
        assert_eq!(process_book_mode(), BookMode::Classic);
    }
}
