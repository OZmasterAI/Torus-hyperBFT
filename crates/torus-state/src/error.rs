use std::fmt;

/// Errors from the state layer.
#[derive(Debug)]
pub enum StateError {
    /// RocksDB operation failed.
    RocksDb(rocksdb::Error),
    /// Data in the database is malformed.
    InvalidData(String),
    /// A required column family is missing.
    MissingColumnFamily(String),
    /// IO error (snapshot operations).
    Io(std::io::Error),
    /// Snapshot verification failed.
    SnapshotVerificationFailed(String),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RocksDb(e) => write!(f, "rocksdb: {e}"),
            Self::InvalidData(msg) => write!(f, "invalid data: {msg}"),
            Self::MissingColumnFamily(name) => write!(f, "missing column family: {name}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::SnapshotVerificationFailed(msg) => {
                write!(f, "snapshot verification failed: {msg}")
            }
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RocksDb(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for StateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rocksdb::Error> for StateError {
    fn from(e: rocksdb::Error) -> Self {
        Self::RocksDb(e)
    }
}

// revm requires the Database error type to implement DBErrorMarker.
impl revm::database_interface::DBErrorMarker for StateError {}
