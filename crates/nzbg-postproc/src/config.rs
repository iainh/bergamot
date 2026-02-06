use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub par2_path: PathBuf,
    pub dest_dir: PathBuf,
    pub append_category_dir: bool,
    pub password_file: Option<PathBuf>,
    pub unpack_cleanup_disk: bool,
    pub ext_cleanup_disk: String,
}

pub use nzbg_core::models::PostStrategy;
