//! Release-independent protocol negotiation. Semantic additions require a new version.
use crate::error::ErrorCode;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Highest implemented protocol version. Version 1 starts the new compatibility baseline.
pub const PROTOCOL: u32 = 1;
/// Signature format version; changing serialized signed records requires a new format.
pub const SIGNATURE_FORMAT: u32 = 1;

/// Inclusive implemented protocol range, exchanged before any operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolRange {
    /// Oldest implemented version; zero and inverted ranges are invalid.
    pub min: u32,
    /// Newest implemented version.
    pub max: u32,
}
impl ProtocolRange {
    /// Supported versions. The initial protocol has no historical predecessor.
    pub const CURRENT: Self = Self {
        min: 1,
        max: PROTOCOL,
    };

    /// Select the highest common version, rejecting malformed or disjoint ranges.
    pub fn negotiate(self, peer: Self) -> Result<u32> {
        let selected = self.max.min(peer.max);
        if self.min == 0
            || peer.min == 0
            || self.min > self.max
            || peer.min > peer.max
            || selected < self.min.max(peer.min)
        {
            bail!(ErrorCode::VersionMismatch.error("no common supported protocol"));
        }
        Ok(selected)
    }

    /// Verify the peer selected the same highest common version.
    pub fn confirm(self, peer: Self, selected: u32) -> Result<()> {
        if self.negotiate(peer)? != selected {
            bail!(ErrorCode::VersionMismatch.error("invalid negotiated protocol"));
        }
        Ok(())
    }

    /// Require a negotiated version before sending a feature with execution semantics.
    pub fn require(selected: u32, introduced: u32) -> Result<()> {
        Self::CURRENT.negotiate(Self {
            min: selected,
            max: selected,
        })?;
        if selected < introduced {
            bail!(ErrorCode::VersionMismatch.error("operation requires a newer protocol"));
        }
        Ok(())
    }

    pub(crate) fn header(self) -> String {
        format!("{}-{}", self.min, self.max)
    }

    pub(crate) fn from_header(value: &str) -> Result<Self> {
        let parsed = value.split_once('-').and_then(|(min, max)| {
            if !min.bytes().all(|byte| byte.is_ascii_digit())
                || !max.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            Some(Self {
                min: min.parse().ok()?,
                max: max.parse().ok()?,
            })
        });
        let Some(range) = parsed else {
            bail!(ErrorCode::VersionMismatch.error("missing or invalid protocol range"));
        };
        range.negotiate(range)?;
        Ok(range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiate_highest_common_and_reject_invalid_ranges() {
        let current = ProtocolRange { min: 2, max: 3 };
        assert_eq!(
            current.negotiate(ProtocolRange { min: 1, max: 2 }).unwrap(),
            2
        );
        for peer in [
            ProtocolRange { min: 0, max: 3 },
            ProtocolRange { min: 4, max: 3 },
            ProtocolRange { min: 1, max: 1 },
            ProtocolRange { min: 4, max: 5 },
        ] {
            let error = current.negotiate(peer).unwrap_err();
            assert!(crate::error::is(&error, ErrorCode::VersionMismatch));
        }
        assert!(current.confirm(current, 2).is_err());
        assert!(ProtocolRange::require(1, 2).is_err());
        assert!(ProtocolRange::require(1, 1).is_ok());
        for invalid in [
            "",
            "1",
            "-1-2",
            "0-1",
            "2-1",
            "1-2-3",
            "1-+2",
            "1-4294967296",
        ] {
            assert!(ProtocolRange::from_header(invalid).is_err());
        }
    }
}
