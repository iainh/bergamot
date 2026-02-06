mod error;
mod model;
mod parser;

pub use crate::error::NzbError;
pub use crate::model::{
    NzbFile,
    NzbInfo,
    NzbMeta,
    ParStatus,
    Segment,
};
pub use crate::parser::{
    classify_par,
    compute_content_hash,
    compute_name_hash,
    extract_filename,
    parse_nzb_auto,
    NzbParser,
};
