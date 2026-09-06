#!/usr/bin/env python3
"""将 scripts/control_page.html 注入 linux/src/report_server.rs 的
CONTROL_PAGE_HTML 常量，并把 stream_mjpeg 改为「最新帧信箱」模型、
补 use。锚点定位，与格式无关。"""

import sys

RS = "/home/z/my-project/cloudphonekeep/linux/src/report_server.rs"
HTML = "/home/z/my-project/cloudphonekeep/scripts/control_page.html"

src = open(RS, encoding="utf-8").read()
html = open(HTML, encoding="utf-8").read().rstrip("\n")

# ── 1) 替换 CONTROL_PAGE_HTML 块 ──────────────────────────────
start_anchor = 'const CONTROL_PAGE_HTML: &str = r#"'
end_anchor = '"#;'
i = src.index(start_anchor)
j = src.index(end_anchor, i)          # 第一个 "#; = 块结束
new_block = start_anchor + html + end_anchor
src = src[:i] + new_block + src[j + len(end_anchor):]

# ── 2) use 补 FramePoll ──────────────────────────────────────
if "use crate::cdp::FramePoll;" not in src:
    src = src.replace(
        "use crate::engine::{health_json, ControlRequest, SharedState};",
        "use crate::cdp::FramePoll;\nuse crate::engine::{health_json, ControlRequest, SharedState};",
        1,
    )

# ── 3) stream_mjpeg：改信箱模型 ──────────────────────────────
sm_start = src.index("fn stream_mjpeg(")
# doc 注释起点（fn 前的 /// 段）
doc_start = src.rindex("/// 实时画面流", 0, sm_start)
# 函数结束锚：stream_mjpeg 尾部的 detach + 闭合
tail = "    screencast_detach(ctrl, sub_id);\n}\n"
sm_end = src.index(tail, sm_start) + len(tail)

new_sm = '''fn stream_mjpeg(stream: &mut TcpStream, ctrl: &Sender<ControlRequest>, logger: &Arc<Logger>) {
    const BOUNDARY: &str = "cpkframe";
    // 1) 订阅引擎实时画面：引擎线程可能正在慢 eval（弱机 tick/采样可达数秒）/
    //    启动浏览器，宽限 8s 再判超时；引擎彻底不可用会立刻 500
    let (tx, rx) = std::sync::mpsc::channel();
    if ctrl.send(ControlRequest::ScreencastAttach { reply: tx }).is_err() {
        respond(stream, 500, "text/plain; charset=utf-8", "引擎不可用".as_bytes());
        return;
    }
    let (sub_id, frame_box) = match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            respond(
                stream,
                500,
                "text/plain; charset=utf-8",
                format!("画面流建立失败：{e}").as_bytes(),
            );
            return;
        }
        Err(_) => {
            respond(
                stream,
                504,
                "text/plain; charset=utf-8",
                "画面流建立超时（引擎忙或浏览器启动中）".as_bytes(),
            );
            return;
        }
    };
    // 2) 流头 + 帧循环（multipart；每次写失败即客户端已断开）。
    //    帧信箱只存最新帧：引擎覆盖写入（丢旧保新），此处 poll 等待/取帧
    let head = format!(
        "HTTP/1.1 200 OK\\r\\nContent-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\\r\\nCache-Control: no-store\\r\\nConnection: close\\r\\n\\r\\n"
    );
    if stream.write_all(head.as_bytes()).is_err() {
        screencast_detach(ctrl, sub_id);
        return;
    }
    let mut last: Option<Vec<u8>> = None;
    let mut last_push = Instant::now();
    let opened = Instant::now();
    loop {
        match frame_box.poll(Duration::from_secs(2)) {
            FramePoll::Frame(frame) => {
                if !write_part(stream, BOUNDARY, &frame) {
                    break;
                }
                last = Some(frame);
                last_push = Instant::now();
            }
            FramePoll::Idle => {
                // 静态页：生产侧活着但无新帧 → 心跳重发上一帧维持连接
                // （路由器/代理不掐空闲连接，页面也不闪重连循环）
                if let Some(f) = &last {
                    if last_push.elapsed() >= Duration::from_secs(2) {
                        if !write_part(stream, BOUNDARY, f) {
                            break;
                        }
                        last_push = Instant::now();
                    }
                } else if opened.elapsed() > Duration::from_secs(30) {
                    // 首帧 30s 未至（引擎极端繁忙/浏览器启动中）→ 关流，页面转截图兜底
                    logger.log(1, "sys", "实时画面流首帧 30s 未至，关流（页面自动转截图轮询并重连）");
                    break;
                }
            }
            // 生产侧心跳丢失：CDP 会话重建/浏览器重启，旧信箱不会再有帧
            // → 立即关流，页面 1.5s 重连新订阅（替代旧 mpsc 的 Disconnected 信号）
            FramePoll::Dead => {
                logger.log(1, "sys", "实时画面流生产侧心跳丢失，关流重连（CDP 会话重建/浏览器重启）");
                break;
            }
        }
    }
    screencast_detach(ctrl, sub_id);
}
'''

new_doc = '''/// 实时画面流：向引擎订阅 screencast 帧信箱（只存最新帧），以 multipart/x-mixed-replace
/// 推送（MJPEG）。退出条件：客户端断开（写失败）/ 生产侧心跳丢失（CDP 重建、
/// 浏览器重启：连续两个窗口无引擎泵心跳）/ 首帧 30s 未至（引擎极端繁忙）。
/// 关流后页面侧自动重连。静态页面合成器无更新 → screencast 不发新帧：以 2s
/// 心跳重发上一帧维持连接。
'''
src = src[:doc_start] + new_doc + new_sm + src[sm_end:]

# ── 4) 清理不再使用的 RecvTimeoutError import（若已无引用）────
if "RecvTimeoutError" not in src.replace(
    "use std::sync::mpsc::{RecvTimeoutError, Sender};", ""
):
    src = src.replace(
        "use std::sync::mpsc::{RecvTimeoutError, Sender};",
        "use std::sync::mpsc::Sender;",
        1,
    )

open(RS, "w", encoding="utf-8").write(src)

# ── 自检 ─────────────────────────────────────────────────────
chk = open(RS, encoding="utf-8").read()
assert "fn stream_mjpeg(" in chk
assert "FramePoll::Frame(frame)" in chk
assert "homei" in chk, "新控制页未注入"
assert "上滑" not in chk and "下滑" not in chk, "方向按钮未删除"
assert "说明</h2>" not in chk, "说明区块未删除"
assert '<header' not in chk, "顶栏未删除"
print("OK: 控制页注入 + stream_mjpeg 信箱化完成")
