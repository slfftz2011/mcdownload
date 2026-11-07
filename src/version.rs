use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "mainClass")]
    pub main_class: Option<String>,
    pub assets: Option<String>,
    #[serde(rename = "assetIndex")]
    pub asset_index: Option<AssetIndexRef>,
    pub downloads: Option<VersionDownloads>,
    #[serde(default)]
    pub libraries: Vec<Library>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct VersionDownloads {
    pub client: Option<DownloadInfo>,
    pub server: Option<DownloadInfo>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DownloadInfo {
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub natives: Option<HashMap<String, String>>,
    pub rules: Option<Vec<Rule>>,
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LibraryDownloads {
    pub artifact: Option<DownloadInfo>,
    pub classifiers: Option<HashMap<String, DownloadInfo>>,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
}

#[derive(Debug, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

impl VersionJson {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("解析 version.json 失败")
    }
}

pub fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

pub fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        ""
    }
}

fn rules_allow(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn rule_matches(rule: &Rule) -> bool {
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if name != current_os() {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            if arch != current_arch() {
                return false;
            }
        }
    }
    true
}

/// `group:artifact:version[:classifier]` → Maven 仓库相对路径
pub fn maven_path(name: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied();

    let filename = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(PathBuf::from(format!("{group}/{artifact}/{version}/{filename}")))
}

/// 一个待下载项
#[derive(Debug, Clone)]
pub struct LibTarget {
    pub url: String,
    pub rel_path: PathBuf,
    pub sha1: String,
    pub size: u64,
    pub label: String,
}

/// 展开 libraries：应用 rules，取 artifact + natives classifier
pub fn collect_libraries(libs: &[Library], maven_base: &str) -> Vec<LibTarget> {
    let mut out = Vec::new();
    let os = current_os();

    for lib in libs {
        if let Some(rules) = &lib.rules {
            if !rules_allow(rules) {
                continue;
            }
        }

        // 主 artifact
        if let Some(dl) = &lib.downloads {
            if let Some(art) = &dl.artifact {
                let rel = maven_path(&lib.name).unwrap_or_else(|| {
                    PathBuf::from(art.url.rsplit('/').next().unwrap_or("unknown.jar"))
                });
                out.push(LibTarget {
                    url: art.url.clone(),
                    rel_path: rel,
                    sha1: art.sha1.clone(),
                    size: art.size,
                    label: lib.name.clone(),
                });
            }

            // natives classifier（老版本需要，只下不解压）
            if let (Some(natives), Some(classifiers)) = (&lib.natives, &dl.classifiers) {
                if let Some(clf) = natives.get(os) {
                    let clf = clf.replace("${arch}", if cfg!(target_arch = "x86") { "32" } else { "64" });
                    if let Some(info) = classifiers.get(&clf) {
                        let mut rel = maven_path(&lib.name).unwrap_or_else(|| {
                            PathBuf::from(info.url.rsplit('/').next().unwrap_or("native.jar"))
                        });
                        let file = rel.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        let new_file = if file.ends_with(".jar") {
                            file.replacen(".jar", &format!("-{clf}.jar"), 1)
                        } else {
                            format!("{file}-{clf}")
                        };
                        rel.set_file_name(new_file);
                        out.push(LibTarget {
                            url: info.url.clone(),
                            rel_path: rel,
                            sha1: info.sha1.clone(),
                            size: info.size,
                            label: format!("{} ({clf})", lib.name),
                        });
                    }
                }
            }
        } else if let Some(rel) = maven_path(&lib.name) {
            // 老格式：没有 downloads，只有 name + 可选 url
            let base = lib.url.as_deref().unwrap_or(maven_base);
            let url = format!("{}/{}", base.trim_end_matches('/'), rel.to_string_lossy());
            out.push(LibTarget {
                url,
                rel_path: rel,
                sha1: String::new(),
                size: 0,
                label: lib.name.clone(),
            });
        }
    }
    out
}