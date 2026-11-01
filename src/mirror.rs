use anyhow::Result;

#[derive(Clone, Debug)]
pub struct MirrorConfig {
    pub enabled: bool,
    pub version_manifest: String,
    pub maven: String,
    pub assets: String,
    pub piston_meta: String,
    pub launcher: String,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            version_manifest: "https://bmclapi2.bangbang93.com/mc/game/version_manifest.json".into(),
            maven: "https://bmclapi2.bangbang93.com/maven".into(),
            assets: "https://bmclapi2.bangbang93.com/assets".into(),
            piston_meta: "https://bmclapi2.bangbang93.com".into(),
            launcher: "https://bmclapi2.bangbang93.com".into(),
        }
    }
}

impl MirrorConfig {
    pub fn official() -> Self {
        Self {
            enabled: false,
            version_manifest: "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json".into(),
            maven: "https://libraries.minecraft.net".into(),
            assets: "https://resources.download.minecraft.net".into(),
            piston_meta: "https://piston-meta.mojang.com".into(),
            launcher: "https://launcher.mojang.com".into(),
        }
    }

    pub fn rewrite(&self, url: &str) -> String {
        if !self.enabled {
            return url.to_string();
        }
        url.replace("https://piston-meta.mojang.com", &self.piston_meta)
            .replace("https://launcher.mojang.com", &self.launcher)
            .replace("https://libraries.minecraft.net", &self.maven)
            .replace("https://resources.download.minecraft.net", &self.assets)
    }

    pub fn rewrite_version_manifest(&self, manifest: &mut serde_json::Value) -> Result<()> {
        if let Some(versions) = manifest.get_mut("versions").and_then(|v| v.as_array_mut()) {
            for v in versions {
                // 关键改动：用闭包替代 str::to_string，让编译器知道收到的是 &str
                if let Some(url) = v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()) {
                    let rewritten = self.rewrite(&url);
                    v["url"] = serde_json::Value::String(rewritten);
                }
            }
        }
        Ok(())
    }
}