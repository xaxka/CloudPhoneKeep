#!/usr/bin/env python3
"""同步 doc/linux-deploy.md 控制台章节（新版控制页）+ README 相关行"""
import re

DOC = "/home/z/my-project/cloudphonekeep/doc/linux-deploy.md"
src = open(DOC, encoding="utf-8").read()

new_section = """## 首次登录与控制台

浏览器打开 `http://127.0.0.1:8088/`：**全屏实时画面 + 极简操作**。
桌面端右侧是操作栏（状态/输入/按键/回首页/全屏）；**手机等窄屏**点击底部
iOS 风格白色圆点弹出底部抽屉控制台（再点圆点或点遮罩收起）。画面右上角
小胶囊徽标显示状态色点与 `N fps`（切后台显示「已暂停」，重连期显示「等帧…」）。

画面为 `Page.startScreencast` 合成器帧直推的 MJPEG 流（页面有更新即出帧，
局域网延迟 ≈ 帧间隔；带动画的页面可达 25-60fps，弱机实际由 CPU/网络决定）。
**触摸完全跟手**：按下/移动/抬起实时注入（CDP `Input.dispatchTouchEvent`，
fire 即答不排队）：拖列表/拉滑块/下拉刷新即时响应，按住不动＝长按；轻点由
Chromium 手势识别自动合成 click；按下时画面上会出现跟随手指的触摸反馈点。
控制端点 `POST /touch`（`phase=start/move/end/cancel` + `x/y`）也可外部脚本直调。
抽屉/操作栏可输入文本、Enter/删除、刷新、回首页、全屏。
VLC 等标准播放器也可直接打开 `http://<host>:<port>/stream.mjpg` 观看。

**切后台/锁屏自动省 CPU**：控制页不可见即断流，引擎最后一个订阅者离开后
自动 `Page.stopScreencast`——无人观看＝零 JPEG 编码开销，CPU 即降（云机页面
本身的运行开销仍在，那是保活语义）；回前台自动重连。关闭标签页同理。

流的连接语义（弱机/慢网络均按此设计，画面右上角徽标显示 `N fps`）：

- **首帧**：订阅即由引擎补发一张当前截图（静态页/错误页合成器无更新时
  screencast 不发帧，兜底保证打开就有画面），连接建立为亚秒级
- **丢旧保新**：引擎帧信箱只存最新帧，消费慢时跳过中间帧——画面永远最新，
  不排队不积压（宁可跳帧，不延迟）
- **静态页面**：无新帧时每 2s 重发上一帧作心跳——连接保持活性，
  徽标显示 0-1 fps 属正常（画面没变就没有新帧，不是卡顿）
- **断流自愈**：引擎重建/浏览器重启（生产侧心跳丢失即判死）会立即关流，
  页面 1.5s 内自动重连；期间画面保留（不闪全屏「连接实时画面…」），
  徽标提示 `等帧…`
- **极端繁忙**（引擎 8s 未应答订阅）：页面退化为逐帧截图轮询
  （上一张完成才发下一张，弱机不会把引擎通道灌爆），8s 后自动重试实时流
"""

# 定位旧章节：从「## 首次登录与控制页」到「## 自动恢复分级」前
start = src.index("## 首次登录与控制页")
end = src.index("## 自动恢复分级")
src = src[:start] + new_section + "\n" + src[end:]

open(DOC, "w", encoding="utf-8").write(src)

# README 一行简介同步（若提及控制页形态）
R = "/home/z/my-project/cloudphonekeep/linux/README.md"
r = open(R, encoding="utf-8").read()
r2 = r.replace(
    "左侧实时画面 + 右侧操作栏",
    "全屏实时画面（手机端 iOS 圆点呼出抽屉控制台）",
)
if r2 != r:
    open(R, "w", encoding="utf-8").write(r2)
    print("README 同步完成")
print("linux-deploy.md 控制台章节已更新")
