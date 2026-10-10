//! Relay-observed ciphertext usage, independent of endpoint jobs and file sizes.
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// UTC calendar period used by relay traffic queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficPeriod {
    /// Current UTC calendar day.
    Today,
    /// Current UTC calendar month.
    #[default]
    Month,
    /// All traffic recorded by this relay deployment.
    All,
}
impl fmt::Display for TrafficPeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Today => "today",
            Self::Month => "month",
            Self::All => "all",
        })
    }
}
impl FromStr for TrafficPeriod {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "today" => Ok(Self::Today),
            "month" => Ok(Self::Month),
            "all" => Ok(Self::All),
            _ => Err("expected today, month or all".into()),
        }
    }
}

/// Paginated traffic query. The relay derives access scope from the proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficQuery {
    /// UTC calendar period; month by default.
    #[serde(default)]
    pub period: TrafficPeriod,
    /// Number of device rows to skip, in immutable device ID order.
    #[serde(default)]
    pub offset: u32,
    /// Maximum device rows, between 1 and 256.
    #[serde(default = "traffic_page_size")]
    pub limit: u16,
}
fn traffic_page_size() -> u16 {
    50
}
impl Default for TrafficQuery {
    fn default() -> Self {
        Self {
            period: TrafficPeriod::Month,
            offset: 0,
            limit: traffic_page_size(),
        }
    }
}
impl TrafficQuery {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if !(1..=256).contains(&self.limit) {
            anyhow::bail!(
                crate::error::ErrorCode::InvalidRequest
                    .error("traffic limit must be between 1 and 256")
            );
        }
        Ok(())
    }
}

/// Binary tunnel payload bytes at the relay; outer control messages are excluded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficBytes {
    /// Ciphertext received from devices, including a valid frame whose forwarding fails.
    pub ingress_bytes: u64,
    /// Ciphertext accepted by the outbound WebSocket sender; not a delivery receipt.
    pub egress_bytes: u64,
}
/// A device's traffic across both initiating and target roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceTraffic {
    /// Authenticated immutable device ID. Anonymous pairing is not assigned an identity.
    pub device_id: String,
    /// Ciphertext this device sent to the relay.
    pub sent_bytes: u64,
    /// Ciphertext the relay submitted to this device.
    pub received_bytes: u64,
}
/// One UTC day's traffic, with missing days represented by zero counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficDay {
    /// UTC day boundary in Unix milliseconds.
    pub start_ms: i64,
    /// Traffic observed during this day.
    #[serde(flatten)]
    pub bytes: TrafficBytes,
}
/// Snapshot reported by the relay, which sees encrypted lengths rather than business content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficReport {
    /// Network root fingerprint used by this relay.
    pub network_id: String,
    /// None for a manager's network report; otherwise the querying member's device ID.
    pub device_id: Option<String>,
    /// Requested UTC calendar period.
    pub period: TrafficPeriod,
    /// Inclusive query boundary; zero means all recorded traffic.
    pub start_ms: i64,
    /// Time of this snapshot, in Unix milliseconds.
    pub end_ms: i64,
    /// Earliest observed frame for this report's network/device, if any.
    pub recorded_since_ms: Option<i64>,
    /// False if recording failed; counters must then be treated as incomplete.
    pub complete: bool,
    /// Totals for the requested period, independent of device pagination.
    pub totals: TrafficBytes,
    /// Device usage in immutable ID order, limited to this query's authorized scope.
    pub devices: Vec<DeviceTraffic>,
    /// Daily trend for this period; all-time reports show the latest 30 UTC days.
    pub daily: Vec<TrafficDay>,
    /// Offset for the next page of devices, if one exists.
    pub next_offset: Option<u32>,
}
