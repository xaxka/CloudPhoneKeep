#!/usr/bin/env python3
"""校验仓库内所有 Markdown 相对链接目标文件是否存在（文档冒烟档）。

用法：
    python3 check_md_links.py            # 检查本脚本所在的仓库根
    python3 check_md_links.py /some/dir  # 检查任意目录（可装 cpk 之外的仓库）
退出码 0 = 全部链接有效；1 = 存在失效链接（逐条打印）。
"""
import os, re, sys

# 仓库根 = 本脚本所在 cli/tests/ 向上两级；首参可覆盖（任意目录可用）
ROOT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1
                       else os.path.join(os.path.dirname(__file__), "..", ".."))

SKIP_DIRS = ("target", ".git", "node_modules", "dist", "build")
md_files = []
for dirpath, dirnames, filenames in os.walk(ROOT):
    if any(p in dirpath for p in SKIP_DIRS):
        continue
    for f in filenames:
        if f.endswith(".md"):
            md_files.append(os.path.join(dirpath, f))

link_re = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
broken = []
checked = 0
for md in md_files:
    base = os.path.dirname(md)
    text = open(md, encoding="utf-8").read()
    for m in link_re.finditer(text):
        target = m.group(1).strip()
        if target.startswith(("http://", "https://", "#", "mailto:")):
            continue
        path = target.split("#")[0]
        if not path:
            continue
        full = os.path.normpath(os.path.join(base, path))
        checked += 1
        if not os.path.exists(full):
            broken.append(f"{os.path.relpath(md, ROOT)} -> {target}")

print(f"检查 {len(md_files)} 个 md 文件，{checked} 条相对链接")
if broken:
    print("失效链接：")
    for b in broken:
        print("  " + b)
    sys.exit(1)
print("全部链接有效")
