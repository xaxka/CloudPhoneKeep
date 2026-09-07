#!/usr/bin/env node
// 墙钟门控语义验证（保活脚本 actionTick 门控，shared/keepalive.inject.js）
// =====================================================================
// 背景：宿主空闲降频（Linux 无观看时 tick 1s→5s）下，旧的计数门控
// （++state.n >= every）会把保活动作周期从 5s 拉长到 every 倍（25s）。
// 修复：墙钟门控（Date.now() >= nextActionAt）——任意 tick 周期
// （≤ intervalMs）下动作周期恒 ≈ intervalMs。
// 本脚本逐字复刻门控逻辑，模拟三种 tick 周期验证周期不漂移。
const CFG = { intervalMs: 5000 };
const state = { nextActionAt: 0 };

function tickGate(nowMs) {
  const period = CFG.intervalMs || 5000;
  if (!state.nextActionAt) state.nextActionAt = nowMs + period;
  return nowMs >= state.nextActionAt ? ((state.nextActionAt = nowMs + period), true) : false;
}

function simulate(label, tickMs, totalMs) {
  state.nextActionAt = 0;
  let fired = 0, firstFire = -1, lastFire = -1;
  for (let t = 0; t <= totalMs; t += tickMs) {
    if (tickGate(t)) { fired++; if (firstFire < 0) firstFire = t; lastFire = t; }
  }
  const avg = fired > 1 ? (lastFire - firstFire) / (fired - 1) : NaN;
  console.log(`${label}: 触发 ${fired} 次，平均周期 ${Math.round(avg)}ms（期望 ~5000ms）`);
  if (fired < 3) { console.error(`${label}: 触发次数过少`); process.exit(1); }
  if (Math.abs(avg - 5000) > 1000) { console.error(`${label}: 周期偏离期望`); process.exit(1); }
}

simulate('活跃态 tick=1000ms', 1000, 60000);   // 旧计数门控：正好 5s（兼容）
simulate('空闲态 tick=5000ms', 5000, 60000);   // 旧计数门控：25s（本修复目标）
simulate('空闲态 tick=2000ms', 2000, 60000);   // 量化容差内（4-6s）
console.log('PASS: 墙钟门控在任意 tick 周期下动作周期恒 ≈ intervalMs');
