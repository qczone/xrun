//! Manager authority, verified records and local caches have distinct owners.
mod authority;
mod cache;
mod records;
mod storage;

pub use authority::Manager;
pub use cache::RosterCache;
pub(crate) use records::{Member, Pairing, verify};
pub use records::{ReceiptAck, SignedRoster, device_name, sign};
