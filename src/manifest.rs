use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersion,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Deserialize)]
pub struct LatestVersion {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(rename = "releaseTime")]
    pub release_time: String,
}

impl VersionManifest {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("解析 version_manifest 失败")
    }

    pub fn find(&self, id: &str) -> Option<&VersionEntry> {
        self.versions.iter().find(|v| v.id == id)
    }

    /// 支持 "latest" / "release" / "snapshot" 三个快捷别名
    pub fn resolve(&self, id: &str) -> Result<&VersionEntry> {
        let lookup = match id {
            "latest" | "release" => self.latest.release.as_str(),
            "snapshot" => self.latest.snapshot.as_str(),
            other => other,
        };
        self.find(lookup)
            .ok_or_else(|| anyhow!("版本不存在: {lookup}"))
    }
}