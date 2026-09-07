//! 平台预设——CLI 与 Tauri 双端唯一源（此前 cli/src/config.rs 与
//! src-tauri/src/config.rs 各自维护一份等值常量，现为共享单一出处）：
//! ```text
//! mobile  → https://cloudphoneh5.buy.139.com       414×896
//! unicom  → https://uphone.wo-adv.cn/cloudphone/#/home  405×720
//! ```
//! 各端对「未知平台」的兜底策略由各端配置层自定（CLI：待机不启动；
//! Tauri：回退 mobile），本模块只提供事实数据。

/// 移动云手机 H5 入口
pub const PLATFORM_MOBILE_URI: &str = "https://cloudphoneh5.buy.139.com";
/// 联通云手机 H5 入口
pub const PLATFORM_UNICOM_URI: &str = "https://uphone.wo-adv.cn/cloudphone/#/home";

pub const PLATFORM_MOBILE_LABEL: &str = "移动云手机";
pub const PLATFORM_UNICOM_LABEL: &str = "联通云手机";

pub const PLATFORM_MOBILE_W: u32 = 414;
pub const PLATFORM_MOBILE_H: u32 = 896;
pub const PLATFORM_UNICOM_W: u32 = 405;
pub const PLATFORM_UNICOM_H: u32 = 720;

/// 平台预设（视口为整数像素；Tauri 侧需要 f64 时在使用处转换）
pub struct PlatformPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub web_uri: &'static str,
    pub width: u32,
    pub height: u32,
}

/// 全部平台预设（数组顺序与两端历史行为一致：mobile 在前，Tauri 未知回退取 [0]）
pub const PLATFORMS: [PlatformPreset; 2] = [
    PlatformPreset {
        id: "mobile",
        label: PLATFORM_MOBILE_LABEL,
        web_uri: PLATFORM_MOBILE_URI,
        width: PLATFORM_MOBILE_W,
        height: PLATFORM_MOBILE_H,
    },
    PlatformPreset {
        id: "unicom",
        label: PLATFORM_UNICOM_LABEL,
        web_uri: PLATFORM_UNICOM_URI,
        width: PLATFORM_UNICOM_W,
        height: PLATFORM_UNICOM_H,
    },
];

/// 按平台 id 精确查找预设；未知平台返回 None（兜底策略由各端决定）
pub fn find(id: &str) -> Option<&'static PlatformPreset> {
    PLATFORMS.iter().find(|p| p.id == id)
}

/// 平台名 → (label, url, w, h)；未知平台 None。
/// CLI 侧历史签名（config::platform_profile）原样保留，调用方零改动
pub fn platform_profile(platform: &str) -> Option<(&'static str, &'static str, u32, u32)> {
    find(platform).map(|p| (p.label, p.web_uri, p.width, p.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consts_match_presets() {
        // 8 个兼容常量与 PLATFORMS 表数值一一对应（防两端历史口径漂移）
        for p in PLATFORMS.iter() {
            let (w, h, uri, label) = match p.id {
                "mobile" => (
                    PLATFORM_MOBILE_W,
                    PLATFORM_MOBILE_H,
                    PLATFORM_MOBILE_URI,
                    PLATFORM_MOBILE_LABEL,
                ),
                "unicom" => (
                    PLATFORM_UNICOM_W,
                    PLATFORM_UNICOM_H,
                    PLATFORM_UNICOM_URI,
                    PLATFORM_UNICOM_LABEL,
                ),
                _ => panic!("未知平台 {}", p.id),
            };
            assert_eq!(p.width, w);
            assert_eq!(p.height, h);
            assert_eq!(p.web_uri, uri);
            assert_eq!(p.label, label);
        }
    }

    #[test]
    fn find_exact_and_unknown() {
        assert_eq!(find("mobile").unwrap().web_uri, PLATFORM_MOBILE_URI);
        assert_eq!(find("unicom").unwrap().label, PLATFORM_UNICOM_LABEL);
        assert!(find("telecom").is_none());
        assert!(find("").is_none());
        assert!(find("Mobile").is_none(), "平台 id 区分大小写");
    }

    #[test]
    fn profile_tuple_shape() {
        let (label, url, w, h) = platform_profile("unicom").unwrap();
        assert_eq!(label, "联通云手机");
        assert_eq!(url, "https://uphone.wo-adv.cn/cloudphone/#/home");
        assert_eq!((w, h), (405, 720));
        assert!(platform_profile("bogus").is_none());
    }

    #[test]
    fn mobile_is_first_for_tauri_fallback() {
        // Tauri 未知平台回退 PLATFORMS[0]：数组顺序是行为约定，锁定防回归
        assert_eq!(PLATFORMS[0].id, "mobile");
    }
}
