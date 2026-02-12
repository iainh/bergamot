//! NNTP (Network News Transfer Protocol) client implementation.
//!
//! Implements the subset of NNTP required for binary article downloading:
//! - Connection and greeting ([RFC 3977 §5.1](https://datatracker.ietf.org/doc/html/rfc3977#section-5.1))
//! - Authentication via AUTHINFO USER/PASS ([RFC 4643 §2.3](https://datatracker.ietf.org/doc/html/rfc4643#section-2.3))
//! - GROUP, BODY, STAT commands ([RFC 3977 §6](https://datatracker.ietf.org/doc/html/rfc3977#section-6))
//! - Multi-line response dot-unstuffing ([RFC 3977 §3.1.1](https://datatracker.ietf.org/doc/html/rfc3977#section-3.1.1))
//! - STARTTLS upgrade ([RFC 4642](https://datatracker.ietf.org/doc/html/rfc4642))

mod error;
pub mod machine;
mod model;
mod pool;
mod protocol;
pub mod scheduler;
mod speed;

pub use crate::error::NntpError;
pub use crate::machine::{
    Event as NntpEvent, Input as NntpInput, NntpMachine, Output as NntpOutput,
    ProtoError as NntpProtoError,
};
pub use crate::model::{Encryption, IpVersion, NewsServer, NntpResponse};
pub use crate::pool::{
    ConnectionFactory, RealConnectionFactory, ServerPool, ServerPoolManager, StatsRecorder,
};
pub use crate::protocol::{BodyReader, NntpConnection, NntpIo, NntpStream};
pub use crate::scheduler::{ServerScheduler, ServerSlot};
pub use crate::speed::{SpeedLimiter, SpeedLimiterHandle};
