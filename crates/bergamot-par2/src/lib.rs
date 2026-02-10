pub mod error;
pub mod format;
pub mod galois;
pub mod model;
pub mod parser;
pub mod repair;
pub mod verify;

pub use error::{Par2ParseError, Par2RepairError, Par2VerifyError};
pub use model::{
    FileId, FileVerifyResult, FileVerifyStatus, Md5Digest, Par2FileEntry, RecoverySet, VerifyResult,
};
pub use parser::parse_recovery_set;
pub use repair::{repair_recovery_set, repair_recovery_set_with_progress};
pub use verify::{verify_recovery_set, verify_recovery_set_with_progress, verify_recovery_set_with_progress_range};
