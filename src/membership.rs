//! Manager authority, verified records and local caches have distinct owners.
pub(crate) const INVITATION_LIFETIME: std::time::Duration = std::time::Duration::from_secs(10 * 60);
mod authority;
pub(crate) mod backup;
mod cache;
mod invitations;
mod leases;
mod records;
pub(crate) use leases::DEFAULT_LEASE_MS;
pub use leases::{AuthorityState, SignedLease};
mod storage;

pub use authority::Manager;
pub use cache::RosterCache;
pub(crate) use records::{Member, Pairing, verify};
pub use records::{ReceiptAck, SignedRoster, device_name, sign};
