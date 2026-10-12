//! Cloudflare placement is a best-effort hint for newly created objects only.
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Preferred location for new relay coordination and data objects.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Region {
    /// Choose a location near the connecting edge.
    #[default]
    Auto,
    /// Western North America.
    Wnam,
    /// Eastern North America.
    Enam,
    /// South America.
    Sam,
    /// Western Europe.
    Weur,
    /// Eastern Europe.
    Eeur,
    /// Asia Pacific.
    Apac,
    /// Northeast Asia Pacific.
    ApacNe,
    /// Southeast Asia Pacific.
    ApacSe,
    /// Oceania.
    Oc,
    /// Africa.
    Afr,
    /// Middle East.
    Me,
}
