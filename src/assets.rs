use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct AssetIndex {
    pub objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

impl AssetIndex {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("解析 asset index 失败")
    }

    /// 返回去重后的 object 列表（相同 hash 只下载一次）
    pub fn unique_objects(&self) -> Vec<&AssetObject> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for obj in self.objects.values() {
            if seen.insert(&obj.hash) {
                out.push(obj);
            }
        }
        out
    }
}