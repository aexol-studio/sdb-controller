use super::tidbclusters::*;
impl Default for TidbClusterDiscoveryLivenessProbeType {
    fn default() -> Self {
        Self::Tcp
    }
}

impl Default for TidbClusterDiscoveryReadinessProbeType {
    fn default() -> Self {
        Self::Tcp
    }
}

impl Default for TidbClusterPdMode {
    fn default() -> Self {
        Self::KopiumEmpty
    }
}

impl Default for TidbClusterPdReadinessProbeType {
    fn default() -> Self {
        Self::Tcp
    }
}

impl Default for TidbClusterPdStartUpScriptVersion {
    fn default() -> Self {
        Self::KopiumEmpty
    }
}

impl Default for TidbClusterPdmsName {
    fn default() -> Self {
        Self::Tso
    }
}

impl Default for TidbClusterPdmsReadinessProbeType {
    fn default() -> Self {
        Self::Tcp
    }
}

impl Default for TidbClusterPdmsStartUpScriptVersion {
    fn default() -> Self {
        Self::KopiumEmpty
    }
}

impl Default for TidbClusterPumpReadinessProbeType {
    fn default() -> Self {
        Self::Tcp
    }
}
