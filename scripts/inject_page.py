#!/usr/bin/env python3
"""将 scripts/control_page.html 注入 linux/src/report_server.rs 的
CONTROL_PAGE_HTML 常量（仅替换该块；其余 Rust 逻辑手写维护，避免旧注入器
用陈旧实现覆盖 stream_mjpeg）。锚点定位，与格式无关。"""

RS = "/home/z/my-project/cloudphonekeep/linux/src/report_server.rs"
HTML = "/home/z/my-project/cloudphonekeep/scripts/control_page.html"

src = open(RS, encoding="utf-8").read()
html = open(HTML, encoding="utf-8").read().rstrip("\n")

start_anchor = 'const CONTROL_PAGE_HTML: &str = r#"'
end_anchor = '"#;'
i = src.index(start_anchor)
j = src.index(end_anchor, i)          # 第一个 "#; = 块结束
new_block = start_anchor + html + end_anchor
src = src[:i] + new_block + src[j + len(end_anchor):]

open(RS, "w", encoding="utf-8").write(src)

# ── 自检（只检查注入的 HTML 块，避免匹配到 Rust 源码/测试文本）──
chk = open(RS, encoding="utf-8").read()
i2 = chk.index(start_anchor)
blk = chk[i2:chk.index(end_anchor, i2)]
assert 'const CONTROL_PAGE_HTML: &str = r#"<!doctype html>' in chk
assert chk.count('const CONTROL_PAGE_HTML') == 1, "控制页常量应只有一个"
assert "homei" in blk, "移动端圆点菜单未注入"
assert "kbin" in blk, "键盘输入框未注入"
assert "/kbd" in blk and "/mouse" in blk and "/clip" in blk and "/fps" in blk, "新端点未接线"
assert "id=\"fpsb\"" not in blk, "fps 悬浮徽标应已移除"
assert "sendKey" not in blk, "旧按键按钮函数应已删除"
print("OK: 控制页已注入 report_server.rs")
