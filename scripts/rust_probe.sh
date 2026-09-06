#!/bin/bash
# Rust 探测器：页面 server + chrome + cargo test --ignored（单命令生命周期）
set -e
cd /home/z/my-project/cloudphonekeep
CHROME=/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell

python3 scripts/e2e_page_server.py 18097 &
WSPID=$!
rm -rf /tmp/probe-rust-profile
"$CHROME" --user-data-dir=/tmp/probe-rust-profile --remote-debugging-port=9336 \
  --remote-debugging-address=127.0.0.1 --remote-allow-origins=* --window-size=414,896 \
  --force-device-scale-factor=1 --disable-gpu --no-sandbox --disable-dev-shm-usage \
  about:blank >/dev/null 2>&1 &
CPID=$!
sleep 3
source ~/.cargo/env; cd linux
cargo test --quiet probe_screencast_via_own_ws -- --ignored --nocapture || RC=$?
cd ..
kill $CPID $WSPID 2>/dev/null || true
exit ${RC:-0}
