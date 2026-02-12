//! NNTP (Network News Transfer Protocol) client implementation.
//!
//! Implements the subset of NNTP required for binary article downloading:
//! - Connection and greeting ([RFC 3977 §5.1](https://datatracker.ietf.org/doc/html/rfc3977#section-5.1))
//! - Authentication via AUTHINFO USER/PASS ([RFC 4643 §2.3](https://datatracker.ietf.org/doc/html/rfc4643#section-2.3))
//! - GROUP, BODY, STAT commands ([RFC 3977 §6](https://datatracker.ietf.org/doc/html/rfc3977#section-6))
//! - Command pipelining ([RFC 3977 §3.5](https://datatracker.ietf.org/doc/html/rfc3977#section-3.5))
//! - Multi-line response dot-unstuffing ([RFC 3977 §3.1.1](https://datatracker.ietf.org/doc/html/rfc3977#section-3.1.1))
//! - STARTTLS upgrade ([RFC 4642](https://datatracker.ietf.org/doc/html/rfc4642))
//! - TLS session resumption ([RFC 8446 §2.2](https://datatracker.ietf.org/doc/html/rfc8446#section-2.2), [RFC 5077](https://datatracker.ietf.org/doc/html/rfc5077))
//! - COMPRESS DEFLATE ([RFC 8054](https://datatracker.ietf.org/doc/html/rfc8054))

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
pub use crate::protocol::{BodyReader, NntpConnection, NntpIo, NntpStream, build_tls_config};
pub use crate::scheduler::{ServerScheduler, ServerSlot};
pub use crate::speed::{SpeedLimiter, SpeedLimiterHandle};
