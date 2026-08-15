use crate::config::Settings;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub remote_version: String,
    pub description: String,
    pub download_url: String,
    pub has_update: bool,
    pub error: Option<String>,
}

/// 宽松版本比较：按点分段逐段比较数字
pub fn version_gt(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .trim_start_matches('V')
            .split('.')
            .map(|s| {
                s.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect()
    };
    let va = parse(a);
    let vb = parse(b);
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 拉取在线版本信息。
/// 兼容两种格式：
/// 1. JSON：{ "version": "1.2.0", "description": "...", "url": "https://..." }
/// 2. 纯文本：首行版本号，其余行作为更新说明（原 aardio 更新源格式）
pub async fn fetch_update(settings: &Settings, current: &str) -> UpdateInfo {
    let mut info = UpdateInfo {
        current_version: current.to_string(),
        remote_version: String::new(),
        description: String::new(),
        download_url: settings.download_page.clone(),
        has_update: false,
        error: None,
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            info.error = Some(format!("创建 HTTP 客户端失败: {e}"));
            return info;
        }
    };

    let resp = match client.get(&settings.update_url).send().await {
        Ok(r) => r,
        Err(e) => {
            info.error = Some(format!("请求更新源失败: {e}"));
            return info;
        }
    };

    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            info.error = Some(format!("读取更新信息失败: {e}"));
            return info;
        }
    };

    // 尝试 JSON
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        info.remote_version = v
            .get("version")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        info.description = v
            .get("description")
            .or_else(|| v.get("content"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
            if !u.is_empty() {
                info.download_url = u.to_string();
            }
        }
    } else {
        // 纯文本：首行版本
        let mut lines = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty());
        if let Some(ver) = lines.next() {
            info.remote_version = ver.to_string();
            info.description = lines.collect::<Vec<_>>().join("\n");
        }
    }

    if !info.remote_version.is_empty() {
        info.has_update = version_gt(&info.remote_version, current);
    } else {
        info.error = Some("更新源未返回有效的版本号".into());
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmp_versions() {
        assert!(version_gt("1.1.0", "1.0.9"));
        assert!(version_gt("v2.0", "1.9.9"));
        assert!(!version_gt("1.0.0", "1.0.0"));
        assert!(!version_gt("0.9", "1.0"));
    }
}
