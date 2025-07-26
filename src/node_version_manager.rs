use anyhow::{Context, Result};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::sync::Mutex;
use tokio::time::{Duration, Instant};

lazy_static! {
    static ref VERSION_CACHE: Mutex<VersionCache> = Mutex::new(VersionCache::new());
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeVersion {
    pub version: String,
    pub date: String,
}

#[derive(Debug, Clone)]
struct VersionCache {
    versions: Vec<NodeVersion>,
    last_updated: Option<Instant>,
    cache_duration: Duration,
}

impl VersionCache {
    fn new() -> Self {
        Self {
            versions: Vec::new(),
            last_updated: None,
            cache_duration: Duration::from_secs(86400), // 1 day
        }
    }

    fn is_expired(&self) -> bool {
        match self.last_updated {
            Some(last_updated) => last_updated.elapsed() > self.cache_duration,
            None => true,
        }
    }

    fn update(&mut self, versions: Vec<NodeVersion>) {
        self.versions = versions;
        self.last_updated = Some(Instant::now());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedVersion {
    pub major: u32,
    pub minor: Option<u32>,
    pub patch: Option<u32>,
}

impl ParsedVersion {
    pub fn new(major: u32, minor: Option<u32>, patch: Option<u32>) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn matches(&self, other: &ParsedVersion) -> bool {
        if self.major != other.major {
            return false;
        }

        if let Some(self_minor) = self.minor {
            if let Some(other_minor) = other.minor {
                if self_minor != other_minor {
                    return false;
                }
            } else {
                return false;
            }
        }

        if let Some(self_patch) = self.patch {
            if let Some(other_patch) = other.patch {
                if self_patch != other_patch {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

impl PartialOrd for ParsedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ParsedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.major.cmp(&other.major) {
            Ordering::Equal => {}
            other => return other,
        }

        match (self.minor, other.minor) {
            (Some(a), Some(b)) => match a.cmp(&b) {
                Ordering::Equal => {}
                other => return other,
            },
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => {}
        }

        match (self.patch, other.patch) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    }
}

impl Eq for ParsedVersion {}

pub struct NodeVersionManager {
    client: reqwest::Client,
}

impl NodeVersionManager {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Resolve a version specification like "23", "23.5", "v22.1.0" to a complete version
    pub async fn resolve_version(&self, version_spec: &str, ignore_cached_versions: bool) -> Result<String> {
        let versions = self.fetch_versions(ignore_cached_versions).await?;
        let parsed_spec = self.parse_version_spec(version_spec)?;

        let matching_versions = self.find_matching_versions(&versions, &parsed_spec);

        if matching_versions.is_empty() {
            anyhow::bail!("No Node.js version found matching '{}'", version_spec);
        }

        let latest = matching_versions.last().unwrap();
        Ok(latest.version.trim_start_matches('v').to_string())
    }

    async fn fetch_versions(&self, ignore_cached_versions: bool) -> Result<Vec<NodeVersion>> {
        // Check cache first
        {
            let cache = VERSION_CACHE
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;

            if !ignore_cached_versions && !cache.is_expired() && !cache.versions.is_empty() {
                return Ok(cache.versions.clone());
            }
        }

        let url = "https://nodejs.org/dist/index.json";
        let response = self
            .client
            .get(url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("Failed to fetch Node.js versions")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to fetch versions: HTTP {}", response.status());
        }

        let mut versions: Vec<NodeVersion> = response
            .json()
            .await
            .context("Failed to parse Node.js versions JSON")?;

        // Sort versions (Node.js API returns them in reverse chronological order)
        versions.sort_by(|a, b| {
            let version_a = self.parse_node_version(&a.version).unwrap_or_default();
            let version_b = self.parse_node_version(&b.version).unwrap_or_default();
            version_a.cmp(&version_b)
        });

        {
            let mut cache = VERSION_CACHE
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
            cache.update(versions.clone());
        }

        Ok(versions)
    }

    fn parse_version_spec(&self, spec: &str) -> Result<ParsedVersion> {
        let cleaned = spec.trim().trim_start_matches('v');

        let parts: Vec<&str> = cleaned.split('.').collect();

        match parts.len() {
            1 => {
                let major = parts[0]
                    .parse::<u32>()
                    .context("Invalid major version number")?;
                Ok(ParsedVersion::new(major, None, None))
            }
            2 => {
                let major = parts[0]
                    .parse::<u32>()
                    .context("Invalid major version number")?;
                let minor = parts[1]
                    .parse::<u32>()
                    .context("Invalid minor version number")?;
                Ok(ParsedVersion::new(major, Some(minor), None))
            }
            3 => {
                let major = parts[0]
                    .parse::<u32>()
                    .context("Invalid major version number")?;
                let minor = parts[1]
                    .parse::<u32>()
                    .context("Invalid minor version number")?;
                let patch = parts[2]
                    .parse::<u32>()
                    .context("Invalid patch version number")?;
                Ok(ParsedVersion::new(major, Some(minor), Some(patch)))
            }
            _ => anyhow::bail!("Invalid version specification: {}", spec),
        }
    }

    fn parse_node_version(&self, version: &str) -> Result<ParsedVersion> {
        self.parse_version_spec(version)
    }

    fn find_matching_versions<'a>(
        &self,
        versions: &'a [NodeVersion],
        spec: &ParsedVersion,
    ) -> Vec<&'a NodeVersion> {
        let mut matching = Vec::new();

        for version in versions {
            if let Ok(parsed) = self.parse_node_version(&version.version) {
                if spec.matches(&parsed) {
                    matching.push(version);
                }
            }
        }

        matching.sort_by(|a, b| {
            let version_a = self.parse_node_version(&a.version).unwrap_or_default();
            let version_b = self.parse_node_version(&b.version).unwrap_or_default();
            version_a.cmp(&version_b)
        });

        matching
    }
}

impl Default for ParsedVersion {
    fn default() -> Self {
        Self {
            major: 0,
            minor: Some(0),
            patch: Some(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_spec() {
        let resolver = NodeVersionManager::new();

        assert_eq!(
            resolver.parse_version_spec("23").unwrap(),
            ParsedVersion::new(23, None, None)
        );

        assert_eq!(
            resolver.parse_version_spec("23.5").unwrap(),
            ParsedVersion::new(23, Some(5), None)
        );

        assert_eq!(
            resolver.parse_version_spec("v22.1.0").unwrap(),
            ParsedVersion::new(22, Some(1), Some(0))
        );

        assert_eq!(
            resolver.parse_version_spec("18.17.0").unwrap(),
            ParsedVersion::new(18, Some(17), Some(0))
        );
    }

    #[test]
    fn test_version_matching() {
        let spec = ParsedVersion::new(23, None, None);
        let version1 = ParsedVersion::new(23, Some(0), Some(0));
        let version2 = ParsedVersion::new(23, Some(5), Some(1));
        let version3 = ParsedVersion::new(22, Some(1), Some(0));

        assert!(spec.matches(&version1));
        assert!(spec.matches(&version2));
        assert!(!spec.matches(&version3));
    }

    #[test]
    fn test_version_ordering() {
        let v1 = ParsedVersion::new(18, Some(17), Some(0));
        let v2 = ParsedVersion::new(18, Some(17), Some(1));
        let v3 = ParsedVersion::new(18, Some(18), Some(0));
        let v4 = ParsedVersion::new(19, Some(0), Some(0));

        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v3 < v4);
    }

    #[tokio::test]
    async fn test_version_resolution() {
        let resolver = NodeVersionManager::new();

        // This test requires internet connection
        if let Ok(version) = resolver.resolve_version("18", false).await {
            assert!(version.starts_with("18."));
            println!("Resolved '18' to: {}", version);
        }
    }
}
