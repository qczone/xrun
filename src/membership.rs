//! Manager authority, verified records and local caches have distinct owners.
pub(crate) const INVITATION_LIFETIME: std::time::Duration = std::time::Duration::from_secs(10 * 60);
mod authority;
mod cache;
mod records;
mod storage;

pub use authority::Manager;
pub use cache::RosterCache;
pub(crate) use records::{Member, Pairing, verify};
pub use records::{ReceiptAck, SignedRoster, device_name, sign};
