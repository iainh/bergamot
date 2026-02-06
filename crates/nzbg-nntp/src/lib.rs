mod error;
mod model;
mod protocol;
mod speed;

pub use crate::error::NntpError;
pub use crate::model::{Encryption, IpVersion, NewsServer, NntpResponse};
pub use crate::protocol::{BodyReader, NntpConnection};
pub use crate::speed::SpeedLimiter;
