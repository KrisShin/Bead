// WebView 只做两件事：把用户动作变成 intent 发给 Rust，把 Rust 返回的 state 快照渲染出来。
// 没有任何业务判断留在这里；计时器只是把 hover 防抖的 tick 送给状态机，读取剪贴板由 Rust 决定。

import { renderBoard } from './render.js';

const root = document.getElementById('root');
const invoke = window.__TAURI__?.core?.invoke ?? window.__TAURI__?.invoke;
const HOVER_DELAY_MS = 200;
const LEAVE_GRACE_MS = 800;

let pendingTick = null;
let currentView = null;

// 没有 Tauri 注入时（浏览器打开本页面做视觉快照）渲染固定样例，复用的仍是同一套 renderBoard 与样式。
const SNAPSHOT_VIEW = {
  view: 'expanded',
  locked: false,
  notice: null,
  cards: [
    { lang: 'en', text: '[en] 你好世界', status: 'idle', authoritative: false },
    { lang: 'zh', text: '你好世界', status: 'idle', authoritative: true },
  ],
};

function scheduleTick(view) {
  window.clearTimeout(pendingTick);
  if (view.view === 'hover_pending') {
    pendingTick = window.setTimeout(() => send('tick'), HOVER_DELAY_MS);
  } else if (view.view === 'collapsing') {
    pendingTick = window.setTimeout(() => send('tick'), LEAVE_GRACE_MS);
  }
}

async function send(intent, payload = {}) {
  if (!invoke) return;
  currentView = await invoke(intent, payload);
  draw(currentView);
}

function draw(view) {
  root.innerHTML = renderBoard(view);
  root.dataset.view = view.view;
  scheduleTick(view);

  root.querySelector('#capsule')?.addEventListener('mouseenter', () => send('hover_enter'));
  root.querySelector('#board')?.addEventListener('mouseleave', () => send('hover_leave'));
  root.querySelectorAll('[data-action="copy"]').forEach((button) => {
    button.addEventListener('click', () => send('copy_card', { lang: button.dataset.lang }));
  });
  root.querySelector('[data-action="pin"]')?.addEventListener('click', () => send('pin'));
  root.querySelector('[data-action="refresh"]')?.addEventListener('click', () => send('refresh'));
}

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') send('esc');
});
document.addEventListener('mousedown', (event) => {
  if (!root.contains(event.target)) send('outside_click');
});

if (invoke) {
  send('get_state');
} else {
  draw(SNAPSHOT_VIEW);
}
