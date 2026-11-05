// WebView 只做两件事：把用户动作变成 intent 发给 Rust，把 Rust 返回的 state 快照渲染出来。
// 没有任何业务判断留在这里；计时器只把 hover 防抖的 tick 与 400ms 编辑防抖送给 Rust，
// 读取剪贴板由 Rust 决定。
//
// 编辑期间的渲染策略：textarea 持有焦点且视图仍是 expanded 时跳过整板重渲染，
// 否则每次提交的返回都会替换正在打字的输入框；失焦或收起后的下一帧统一刷新。
// 输入法（IME）守卫：组词中的 Enter/Esc 属于选词操作，不触发提交也不收起面板。

import { renderBoard } from './render.js';

const root = document.getElementById('root');
const invoke = window.__TAURI__?.core?.invoke ?? window.__TAURI__?.invoke;
const HOVER_DELAY_MS = 200;
const LEAVE_GRACE_MS = 800;
const EDIT_DEBOUNCE_MS = 400;

let pendingTick = null;
let pendingTickKind = null;
let pendingEdit = null;
let composing = false;
let lastHtml = null;
let languagesOpen = false;
let lastView = null;
// 每张卡最后一次进入 Rust 的文本：失焦/回车对未变更内容不重复发起同步，
// 也防止编辑期间被跳过的重渲染让旧值覆盖已同步的新值。
const lastCommitted = new Map();

// 没有 Tauri 注入时（浏览器打开本页面做视觉快照）渲染固定样例，复用的仍是同一套 renderBoard 与样式。
const SNAPSHOT_VIEW = {
  view: 'expanded',
  locked: false,
  notice: null,
  cards: [
    { lang: 'en', text: '[en] 你好世界', status: 'idle', authoritative: false },
    { lang: 'ja', text: '[ja] 你好世界', status: 'idle', authoritative: false },
    { lang: 'zh', text: '你好世界', status: 'idle', authoritative: true },
  ],
};

function scheduleTick(view) {
  // 同类计时已在路上时不重排：重绘风暴里反复 clearTimeout 会把 tick 永远推到不期。
  const kind =
    view.view === 'hover_pending' ? 'hover' : view.view === 'collapsing' ? 'grace' : null;
  if (kind !== null && kind === pendingTickKind) return;
  window.clearTimeout(pendingTick);
  pendingTickKind = kind;
  if (kind === 'hover') {
    pendingTick = window.setTimeout(() => {
      pendingTickKind = null;
      send('tick');
    }, HOVER_DELAY_MS);
  } else if (kind === 'grace') {
    pendingTick = window.setTimeout(() => {
      pendingTickKind = null;
      send('tick');
    }, LEAVE_GRACE_MS);
  }
}

async function send(intent, payload = {}) {
  if (!invoke) return;
  const view = await invoke(intent, payload);
  draw(view);
}

async function commitEdit(lang, textarea) {
  if (!root.contains(textarea)) return; // 面板已收起：迟到的提交不得复活已清空的文档
  if (composing) return; // 组词中的拼音不是最终文本，compositionend 会重新排程
  const text = textarea.value;
  if (lastCommitted.get(lang) === text) return;
  lastCommitted.set(lang, text);
  await send('commit_edit', { lang, text });
}

function scheduleEdit(lang, textarea) {
  window.clearTimeout(pendingEdit);
  pendingEdit = window.setTimeout(() => commitEdit(lang, textarea), EDIT_DEBOUNCE_MS);
}

function draw(view) {
  lastView = view;
  const html = renderBoard({ ...view, languagesOpen });
  const unchanged = html === lastHtml;
  const editing =
    document.activeElement?.matches?.('textarea.card__text') ?? false;
  // 渲染结果不变就不动 DOM：光标下的节点一旦被替换，Chromium 会补发 mouseenter，
  // 与 hover_enter→重绘形成风暴，把 tick 防抖永远清掉。编辑中的整板替换会夺走焦点，同样跳过。
  if (unchanged || (editing && view.view === 'expanded')) {
    scheduleTick(view);
    return;
  }
  lastHtml = html;
  root.innerHTML = html;
  root.dataset.view = view.view;
  for (const card of view.cards) lastCommitted.set(card.lang, card.text);

  root.querySelector('#capsule')?.addEventListener('mouseenter', () => send('hover_enter'));
  root.querySelector('#capsule')?.addEventListener('mouseleave', (event) => {
    if (!event.target.isConnected) return; // 节点被替换时的假离开不算真的移出
    send('hover_leave');
  });
  root.querySelector('#board')?.addEventListener('mouseleave', () => send('hover_leave'));

  root.querySelectorAll('textarea.card__text').forEach((textarea) => {
    const lang = textarea.dataset.lang;
    textarea.addEventListener('focus', () => send('edit_start'));
    textarea.addEventListener('input', () => scheduleEdit(lang, textarea));
    textarea.addEventListener('compositionstart', () => {
      composing = true;
    });
    textarea.addEventListener('compositionend', () => {
      composing = false;
      scheduleEdit(lang, textarea);
    });
    textarea.addEventListener('keydown', (event) => {
      if (event.isComposing || composing) return;
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault();
        window.clearTimeout(pendingEdit);
        commitEdit(lang, textarea);
        textarea.blur();
      }
    });
    textarea.addEventListener('blur', () => {
      window.clearTimeout(pendingEdit);
      commitEdit(lang, textarea);
      send('edit_end');
    });
  });

  root.querySelectorAll('[data-action="copy"]').forEach((button) => {
    button.addEventListener('click', () => send('copy_card', { lang: button.dataset.lang }));
  });
  root.querySelectorAll('[data-action="retry"]').forEach((button) => {
    button.addEventListener('click', () => send('retry_failed'));
  });
  root.querySelector('[data-action="languages"]')?.addEventListener('click', () => {
    languagesOpen = !languagesOpen;
    if (lastView) draw(lastView);
  });
  // 面板内勾选即时生效；被禁用的勾选框（会突破 2–5 边界）收不到 change。
  root.querySelectorAll('.board__langs input[type="checkbox"]').forEach((box) => {
    box.addEventListener('change', () => {
      const codes = [...root.querySelectorAll('.board__langs input:checked')].map(
        (el) => el.dataset.langCode,
      );
      send('set_languages', { languages: codes });
    });
  });
  root.querySelector('[data-action="pin"]')?.addEventListener('click', () => send('pin'));
  root.querySelector('[data-action="refresh"]')?.addEventListener('click', () => send('refresh'));
}

document.addEventListener('keydown', (event) => {
  if (event.isComposing) return; // 输入法正在组词（包括用 Esc 取消组词）
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
