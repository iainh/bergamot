mod cleanup;
mod config;
mod error;
mod mover;
mod par2;
mod processor;
mod unpack;

pub use crate::cleanup::cleanup_archives;
pub use crate::config::{Config, PostStrategy};
pub use crate::error::{Par2Error, PostProcessError};
pub use crate::mover::{move_to_destination, resolve_dest_dir};
pub use crate::par2::{NativePar2Engine, Par2CommandLine, Par2Engine, Par2Result};
pub use crate::processor::{
    ExtensionContext, ExtensionExecutor, PostProcessContext, PostProcessRequest, PostProcessor,
    PostStage, find_par2_file,
};
pub use crate::unpack::{
    ArchiveType, CommandLineUnpacker, UnpackResult, Unpacker, detect_archives,
};
