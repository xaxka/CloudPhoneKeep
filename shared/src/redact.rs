//! URL 日志脱敏：双端日志/诊断输出的统一漏斗。
//!
//! 背景：云机页面 URL 常带会话凭证（如 `yun.139.com/ai-helper-phone/?...&token=<凭证>`，
//! 见 cpk-20261004/05.log——页面加载事件与 nav 留痕行把完整 token 落了盘），
//! 而 `shared/docs/diagnostics.md` 承诺「日志不记录任何帐号凭证」。日志天然会被
//! 拿来分享排障，本模块把敏感 query 参数值打码为 `***` 后再输出：
//! - 参数名集合覆盖常见会话/密钥命名（token/session/key/secret/auth/ticket/
//!   password/pwd/signature/sign/code），大小写不敏感
//! - 未命中的参数（phoneId/clientId 等非凭证）原样保留，排障信息量不缩水
//! - 同时覆盖标准 query（`?`）与 SPA hash 内的 query（`#/x?a=b`）
//! - 解析异常绝不丢日志：任何分支失败原样返回
//!
//! 注入脚本（`shared/keepalive.inject.js`）内的 `safeUrl()` 是本逻辑的 JS 同款
//! 实现（页内 diag 输出用），两侧参数名集合保持同步。

/// 敏感参数名（小写精确匹配）
const SENSITIVE_KEYS: [&str; 12] = [
    "token", "session", "sess", "key", "secret", "auth", "ticket", "password", "pwd", "signature",
    "sign", "code",
];

/// 把 URL 中敏感参数的值替换为 `***`，其余原样返回。
///
/// ```text
/// a.com/p?token=ABC&x=1      → a.com/p?token=***&x=1
/// a.com/p#/i?phoneId=1&t=2    → a.com/p#/i?phoneId=1&t=***
/// ```
pub fn redact_url(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    let mut i = 0;
    // 是否处于 ?/# 开头的 query 段内；# 会结束上一段（SPA hash 路由后再遇 ? 重新进入）
    let mut in_query = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'?' || c == b'#' {
            in_query = true;
            out.push(c as char);
            i += 1;
            continue;
        }
        if !in_query {
            out.push(c as char);
            i += 1;
            continue;
        }
        // query 段内：读取一个参数（到 & / # / 串尾）
        let start = i;
        while i < bytes.len() && bytes[i] != b'&' && bytes[i] != b'#' {
            i += 1;
        }
        let param = &url[start..i];
        match param.split_once('=') {
            Some((k, _)) if SENSITIVE_KEYS.contains(&k.to_ascii_lowercase().as_str()) => {
                out.push_str(k);
                out.push_str("=***");
            }
            _ => out.push_str(param),
        }
        if i < bytes.len() {
            let sep = bytes[i];
            if sep == b'#' {
                in_query = false;
            }
            out.push(sep as char);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn token_params_are_masked() {
        assert_eq!(
            redact_url("https://yun.139.com/ai-helper-phone/?clientId=900213&token=YZsidsecret001&targetSourceId=001"),
            "https://yun.139.com/ai-helper-phone/?clientId=900213&token=***&targetSourceId=001"
        );
    }

    #[test]
    fn non_sensitive_params_are_kept() {
        assert_eq!(
            redact_url("https://x.com/#/instance?phoneId=1u7h6151&lockStatus=0&platformType=1"),
            "https://x.com/#/instance?phoneId=1u7h6151&lockStatus=0&platformType=1"
        );
    }

    #[test]
    fn case_insensitive_and_spa_hash_query() {
        assert_eq!(
            redact_url("https://x.com/p#/pay?SIGN=abc&Session=zz"),
            "https://x.com/p#/pay?SIGN=***&Session=***"
        );
    }

    #[test]
    fn plain_url_and_edge_cases_pass_through() {
        assert_eq!(redact_url("https://a.com/#/cloudAppList"), "https://a.com/#/cloudAppList");
        assert_eq!(redact_url(""), "");
        assert_eq!(redact_url("nonsense"), "nonsense");
        assert_eq!(redact_url("a?sign"), "a?sign"); // 无值参数原样
        assert_eq!(redact_url("a?token="), "a?token=***"); // 空值也打码
    }
}
