// WebView 只做两件事：把用户动作变成 intent 发给 Rust，把 Rust 返回的 state 快照渲染出来。
//
// 稳定性约束（改这个文件前先读）：
// 1) #capsule 与 #board-host 是常驻节点，事件全部委托在 #root 上一次绑定。绝不再做
//    root.innerHTML = ...：节点被替换时 Chromium 会补发 mouseenter/leave，hover 触发就抖。
// 2) hover 判定绑在覆盖整个窗口的 #root 上，配合 Rust 侧的真实光标位置复核，
//    窗口从胶囊长成整板时补发的假 leave 不会把面板收回去。
// 3) 每个 IPC 响应带序号，只应用最后一次；后台翻译完成走 state 事件单独推视图。
// 4) 计时只把 hover / grace 的 tick 与 400ms 编辑防抖送给 Rust，业务判断全在 Rust。

import { renderBoard } from './render.js';
import { createEditDebouncer, planEditingRefresh } from './edit-sync.js';
import { Card, LangAutocomplete } from './components/index.js';

const root = document.getElementById('root');
const boardHost = document.getElementById('board-host');
const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke ?? tauri?.invoke;
const listen = tauri?.event?.listen;
const convertFileSrc = tauri?.core?.convertFileSrc;
const WebviewWindow = tauri?.window?.WebviewWindow;

const DEFAULT_HOVER_DELAY_MS = 400;
const DEFAULT_LEAVE_GRACE_MS = 800;
const LEAVE_DEBOUNCE_MS = 200;
// 输入停止 400ms 后自动提交翻译：比键盘节奏慢半拍，又明显快于“思考后失焦”，
// 同时避免每敲一个字就打一次翻译接口。失焦 / Enter 仍然立即提交，并取消这个计时。
const DRAG_GUARD_MS = 3000;

let pendingTick = null;
let pendingTickKind = null;
let leaveTimer = null;
let composing = false;
// 点「清空」会先让正在编辑的卡片失焦；这个窗口内的 blur 不得把旧文本再提交回去。
let suppressCommit = false;
let lastBoardHtml = null;
let lastView = null;
let languagesOpen = false;
let sendSeq = 0;
let dragging = false;
let dragGuard = null;
let currentAudio = null;
// 每张卡最后一次进入 Rust 的文本：失焦/回车对未变更内容不重复发起同步。
const lastCommitted = new Map();
// 输入防抖只记语言，到期时再按语言找回当前输入框（见 edit-sync.js 的说明）。
const editDebouncer = createEditDebouncer({
  onCommit: (lang) => commitPendingEdit(lang),
});

// 没有 Tauri 注入时（浏览器打开本页面做视觉快照）渲染固定样例，复用同一套 renderBoard 与样式。
const SNAPSHOT_VIEW = {
  view: 'expanded',
  locked: false,
  cards: [
    { lang: 'en', text: '[en] 你好世界', status: 'idle', authoritative: false },
    { lang: 'ja', text: '[ja] 你好世界', status: 'idle', authoritative: false },
    { lang: 'zh', text: '你好世界', status: 'idle', authoritative: true },
  ],
  settings: { hover_delay_ms: 400, leave_grace_ms: 800 },
};

async function send(intent, payload = {}) {
  if (!invoke) return null;
  const seq = ++sendSeq;
  try {
    const view = await invoke(intent, payload);
    if (view && seq === sendSeq) draw(view); // 乱序的旧响应不得把视图推回去
    return view ?? null;
  } catch (error) {
    console.warn('[bead]', intent, error);
    return null;
  }
}

// 局部更新专用：拿到最新快照，但不触发整板 draw。
async function sendSilent(intent, payload = {}) {
  if (!invoke) return null;
  const seq = ++sendSeq;
  try {
    const view = await invoke(intent, payload);
    return view && seq === sendSeq ? view : null; // 乱序的旧响应直接丢弃
  } catch (error) {
    console.warn('[bead]', intent, error);
    return null;
  }
}

function scheduleTick(view) {
  // 延迟一律读 Rust 的设置：写死 200ms 时用户把延迟改成 350ms 就永远展开不了。
  const kind =
    view.view === 'hover_pending' ? 'hover' : view.view === 'collapsing' ? 'grace' : null;
  if (dragging) {
    // 拖动胶囊时别在半路把窗口撑开：原生拖动循环和 resize 会互相打架。
    window.clearTimeout(pendingTick);
    pendingTickKind = null;
    return;
  }
  if (kind === pendingTickKind) return;
  window.clearTimeout(pendingTick);
  pendingTickKind = kind;
  if (!kind) return;
  const delay =
    kind === 'hover'
      ? (view.settings?.hover_delay_ms ?? DEFAULT_HOVER_DELAY_MS)
      : (view.settings?.leave_grace_ms ?? DEFAULT_LEAVE_GRACE_MS);
  pendingTick = window.setTimeout(() => {
    pendingTickKind = null;
    send('tick');
  }, delay + 25); // 留一点时钟余量，避免状态机判定「还差几毫秒」白跑一次
}

function clearBoard() {
  if (!boardHost.childElementCount) return;
  boardHost.innerHTML = '';
  lastBoardHtml = null;
}

// --- 窗口形态与面板相位 ---
//
// 窗口只有两种尺寸：胶囊 68×28、面板 613×300，切换一步到位（不做尺寸逐帧动画）。
// 唯一铁律：**resize 只发生在画面不可见的那一帧** —— 展开时先全透明再变大，
// 收起时先淡到 0 再缩回。可见期间改窗口尺寸，WebView 上一帧的白底面板会被留在
// 新窗口里，就是那块偶发的白色「Bead」矩形。
//
// 位置只按胶囊屏幕矩形换算：展开态窗口 x = 胶囊左缘 - 272（整数），
// 因此胶囊在整个开合过程中一个像素都不动（CSS 里 data-shape='panel' 把胶囊放在 272px）。
const ENTER_MOVE_MS = 160;   // 展开：transform 归零（与 closing 对称）
const EXIT_TIMEOUT_MS = 180; // 收起：淡出 140ms + 位移 160ms，180ms 兜底后必须已经缩回
const REVERSE_MS = 200;      // 收起途中回来：从当前值回正
const CARD_STAGGER_MS = 30;
const CARD_STAGGER_MAX_MS = 120;
const CARD_ANIM_MS = 200;

let shape = 'capsule'; // 窗口几何形态：capsule=68×28，panel=613×300
let phase = 'hidden';  // hidden | entering | shown | closing | reversing
let phaseSeq = 0;      // 相位令牌：只有最新一次开合流程可以继续
let closeTimer = null;
let enterDoneTimer = null;
let pendingView = null; // 进入过渡期间的最新快照，过渡结束后再应用
let lastPanelCardCount = 0; // 面板可见期间上次渲染的卡片数，用于检测增删触发宽度同步

function setShape(next) {
  shape = next;
  root.dataset.shape = next;
}

function setPhase(next) {
  phase = next;
  boardHost.dataset.phase = next;
}

function nextFrame() {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

function clearCloseTimer() {
  if (closeTimer) {
    window.clearTimeout(closeTimer);
    closeTimer = null;
  }
}

function clearEnterTimer() {
  if (enterDoneTimer) {
    window.clearTimeout(enterDoneTimer);
    enterDoneTimer = null;
  }
}

function renderContent(view, wasExpanded) {
  // 编辑中不能整板替换（会夺走焦点），但「其余语言卡已经拿到新译文」必须立刻可见：
  // 只刷新不含正在编辑输入框的卡；编辑中的卡文本归用户，整卡保留不动。
  const focusedEditor = document.activeElement?.matches?.('textarea.card__text')
    ? document.activeElement
    : null;
  if (focusedEditor && wasExpanded) {
    refreshOtherCards(view, focusedEditor.dataset.lang);
    return;
  }
  const html = renderBoard({ ...view, languagesOpen });
  // html 没变就不碰 DOM
  if (html === lastBoardHtml) return;
  lastBoardHtml = html;
  boardHost.innerHTML = html;
  for (const card of view.cards ?? []) lastCommitted.set(card.lang, card.text);
}

// 编辑期局部刷新：其余卡整卡替换（焦点在编辑卡上，不会丢光标），
// 正在编辑的卡保持原样，避免打断输入法组字与光标位置。
function refreshOtherCards(view, editingLang) {
  const plan = planEditingRefresh(view, editingLang);
  if (!plan) return;
  const elements = [...boardHost.querySelectorAll('.card')];
  for (const { card, replaceElement } of plan.cards) {
    if (!replaceElement) continue;
    const el = elements.find((node) => node.dataset.lang === card.lang);
    if (!el) continue;
    el.outerHTML = Card({ card, canClose: plan.canClose, usedCodes: plan.usedCodes });
    lastCommitted.set(card.lang, card.text);
  }
  lastBoardHtml = null; // 编辑结束后必须再对齐一次整板
}

// 卡片错峰：第 n 张 delay = min(n*30ms, 120ms)。只写内联延迟，
// 动画名仍是卡片自己的 card-pop-in（卡片模块的增删动画不受影响）。
function applyCardStagger() {
  const cards = boardHost.querySelectorAll('.card');
  cards.forEach((card, index) => {
    const delay = Math.min(index * CARD_STAGGER_MS, CARD_STAGGER_MAX_MS);
    card.style.setProperty('--card-delay', `${delay}ms`);
  });
}

// 展开 68×28 → 613×300（严格六步）
async function beginExpand(view) {
  const token = ++phaseSeq;
  clearCloseTimer();
  clearEnterTimer();
  pendingView = null;

  // 1) 同步写入进入态：内容全透明、宿主隐藏；此时窗口还是 68×28，胶囊仍在屏幕原位
  setPhase('entering');
  boardHost.dataset.visible = 'false';
  boardHost.dataset.motion = 'on';
  // 注意：data-cards='enter' 不能在这里设 —— CSS animation 在 visibility:hidden 上也会跑，
  // 等内容变可见时卡片动画已经跑了 ~50ms，直接从中间"蹦"出来而不是从头进场。
  // 必须延迟到 visible=true 的同一帧再触发（见下面第 3 步末尾）。
  renderContent(view, false);

  // 2) 尺寸切换一步到位：先切到 panel 形态（胶囊随之落到 272px，屏幕上不动），
  //    再让 Rust 用一次 SetWindowPos 提交「位置 + 尺寸」，没有中间几何帧
  setShape('panel');
  if (invoke) {
    await invoke('resize_window', { expanded: true }).catch(() => {});
  }
  if (token !== phaseSeq) return;

  // 3) 连续两帧：保证新尺寸下先提交一帧全透明画面，再把宿主切成可见
  await nextFrame();
  await nextFrame();
  if (token !== phaseSeq) return;
  applyCardStagger();                // 先设好每张卡的错峰延迟
  boardHost.dataset.cards = 'enter'; // 这一帧同时让 board transition 和 card animation 起跑
  boardHost.dataset.visible = 'true';
  setPhase('shown');

  // 5) 过渡走完再解锁内容更新，并把 will-change 撤掉
  enterDoneTimer = window.setTimeout(() => {
    enterDoneTimer = null;
    boardHost.removeAttribute('data-motion');
    // 停在 settled（animation:none），不能直接移除属性：那会让 .card 回退到
    // card-pop-in，所有卡片再播放一次过冲动画，就是展开末尾的「抽一下」。
    boardHost.dataset.cards = 'settled';
    for (const card of boardHost.querySelectorAll('.card')) {
      card.style.removeProperty('--card-delay');
    }
    if (pendingView) {
      const latest = pendingView;
      pendingView = null;
      draw(latest);
    }
  }, Math.max(ENTER_MOVE_MS, CARD_STAGGER_MAX_MS + CARD_ANIM_MS) + 40);
}

// 收起 613×300 → 68×28（内容先出，全部结束之前不碰窗口尺寸）
function beginCollapse() {
  if (phase === 'hidden' || phase === 'closing') return;
  clearEnterTimer();
  pendingView = null;
  const token = ++phaseSeq;
  boardHost.dataset.motion = 'on';
  setPhase('closing');
  // transitionend 为主，180ms 兜底（过渡被打断或元素不参与过渡时也能收回）
  closeTimer = window.setTimeout(() => finishCollapse(token), EXIT_TIMEOUT_MS);
}

async function finishCollapse(token) {
  if (token !== phaseSeq || phase !== 'closing') return;
  clearCloseTimer();
  setPhase('hidden');
  boardHost.dataset.visible = 'false';
  boardHost.removeAttribute('data-cards');

  // 缩回胶囊：这一步发生时内容 opacity 已为 0，宿主也 hidden，画面全透明。
  // Rust 在同一个命令里先关掉命中测试再缩窗口，缩完立刻恢复，不让它吃掉点击。
  if (invoke) {
    await invoke('resize_window', { expanded: false }).catch(() => {});
  }
  if (token !== phaseSeq) return;

  // 窗口已经是 68×28：胶囊切回窗口左缘，屏幕位置与展开前完全一致。
  // 缩窗这一帧胶囊必须「瞬间」出现在原位：不加这个标记时它会从 opacity 0
  // 再走 220ms 的 hover 过渡淡入（实测缩窗帧取样 19 → 54 → 96 → 104），
  // 与「160ms 后画面上只剩胶囊」的验收不符，视觉上也像闪了一下。
  root.dataset.capsuleInstant = '1';
  setShape('capsule');
  await nextFrame();
  await nextFrame();
  delete root.dataset.capsuleInstant;
  boardHost.removeAttribute('data-motion');
  clearBoard();
  lastPanelCardCount = 0;
}

// 面板开着时卡片数变了：协调一次「内容淡出 → Rust resize → 内容淡入」。
// Rust 侧不再直接 resize（可见期 resize 会被 OS 合成器抓拍到旧内容 + 新尺寸的中间帧），
// 改由前端用 opacity 过渡把内容暂时藏起来，resize 完成后再恢复。
let panelWidthSyncing = false;
async function syncPanelWidthSilent() {
  if (!invoke || panelWidthSyncing) return;
  panelWidthSyncing = true;
  boardHost.style.transition = 'opacity 60ms ease';
  boardHost.style.opacity = '0';
  await new Promise((r) => setTimeout(r, 60));
  await invoke('sync_panel_width').catch(() => {});
  await nextFrame();
  await nextFrame();
  boardHost.style.opacity = '1';
  await new Promise((r) => setTimeout(r, 80));
  boardHost.style.transition = '';
  boardHost.style.opacity = '';
  panelWidthSyncing = false;
}

// 收起途中回到胶囊：窗口还在，直接把面板从当前计算值补回满显
function reverseExpansion() {
  clearCloseTimer();
  const token = ++phaseSeq;
  setPhase('reversing');
  boardHost.dataset.motion = 'on';
  window.setTimeout(() => {
    if (token !== phaseSeq || phase !== 'reversing') return;
    setPhase('shown');
    boardHost.removeAttribute('data-motion');
    if (pendingView) {
      const latest = pendingView;
      pendingView = null;
      draw(latest);
    }
  }, REVERSE_MS + 40);
}

boardHost.addEventListener('transitionend', (event) => {
  if (event.propertyName !== 'opacity') return;
  if (event.target?.id !== 'board') return;
  if (phase === 'closing') finishCollapse(phaseSeq);
});

function draw(view) {
  if (!view || typeof view !== 'object') return;
  lastView = view;
  const expanded = view.view === 'expanded';
  const wasExpanded = root.dataset.view === 'expanded';
  root.dataset.view = view.view;

  if (expanded) {
    if (phase === 'entering' || phase === 'reversing') {
      // 过渡中只记下最新快照，等过渡结束再应用：中途重绘会打断/重跑进入动画
      pendingView = view;
    } else if (phase === 'hidden' || phase === 'closing') {
      // hidden = 正常展开；closing = 收起未完成又展开，重走完整流程（含 resize）
      beginExpand(view);
    } else {
      renderContent(view, wasExpanded);
      // 面板开着时卡片数变了：协调一次不可见期的窗口 resize
      const curCount = view.cards?.length ?? 0;
      if (curCount !== lastPanelCardCount && lastPanelCardCount > 0) {
        syncPanelWidthSilent();
      }
      lastPanelCardCount = curCount;
    }
  } else if (view.view === 'hover_pending' && shape === 'panel' && phase !== 'shown' && phase !== 'hidden') {
    // 收起未完成就回来：窗口还在，反向补间回正，不做 resize。
    // entering 时已经在展开，忽略即可（Rust 的 hover 计时到了会再发 expanded）。
    pendingView = view;
    if (phase === 'closing') reverseExpansion();
  } else {
    // collapsing（离开宽限期）/ capsule（ESC、点外部）：内容先出场，再缩窗口
    beginCollapse();
  }
  scheduleTick(view);
}

function requestEnter() {
  if (dragging) return;
  window.clearTimeout(leaveTimer);
  leaveTimer = null;
  if (root.dataset.view === 'expanded') return;
  send('hover_enter');
}

// 展开不再由「鼠标进入窗口」触发：必须同时满足
//   1) 悬停在胶囊上；2) 光标纵坐标贴近胶囊顶端 2px 以内。
// 这样鼠标只是路过胶囊中下部（或贴边滑过）不会弹出面板。
const CAPSULE_TRIGGER_Y = 2;
let edgeArmed = false;

function onCapsuleTopEdge(event) {
  if (event.clientY >= CAPSULE_TRIGGER_Y) return false;
  const capsule = document.getElementById('capsule');
  if (!capsule) return false;
  const rect = capsule.getBoundingClientRect();
  return event.clientX >= rect.left && event.clientX <= rect.right;
}

function requestLeave() {
  if (dragging) return;
  window.clearTimeout(leaveTimer);
  leaveTimer = window.setTimeout(() => {
    leaveTimer = null;
    if (!dragging) send('hover_leave'); // Rust 会复核光标是否真的在窗口外
  }, LEAVE_DEBOUNCE_MS);
}

// 朗读：在线产物走 asset 协议；SAPI 自播（kind=played）不需要前端动作。
function playSpeak(view) {
  const speak = view?.speak;
  if (!speak || speak.kind !== 'file' || !speak.url || !invoke) return;
  let url;
  try {
    url = convertFileSrc
      ? convertFileSrc(speak.url)
      : `asset://localhost/${speak.url.replace(/\\/g, '/')}`;
  } catch {
    return;
  }
  if (!url) return;
  if (!currentAudio) currentAudio = new Audio();
  currentAudio.pause();
  currentAudio.src = url; // 换 src 后自然从头播；复用同一个元素，避免每点一次就泄漏一个 Audio
  currentAudio.play().catch(() => {}); // 朗读是增强功能，失败不打断翻译主流程
}

async function speakCard(lang) {
  const view = await send('speak_card', { lang });
  playSpeak(view);
}

async function commitEdit(lang, textarea) {
  if (root.dataset.view !== 'expanded' || !boardHost.contains(textarea)) return;
  if (composing || suppressCommit) return;
  const text = textarea.value;
  if (lastCommitted.get(lang) === text) return;
  lastCommitted.set(lang, text);
  await send('commit_edit', { lang, text });
}

// 输入停顿到期：按语言找回「当前」输入框再提交。排程时捕获的节点可能已被
// 重渲染替换，contains 检查会让这次提交静默消失（表现就是停多久都不同步）。
function commitPendingEdit(lang) {
  const textarea = [...boardHost.querySelectorAll('textarea.card__text')].find(
    (el) => el.dataset.lang === lang,
  );
  if (textarea) commitEdit(lang, textarea);
}

function cancelAllEdits() {
  editDebouncer.cancelAll();
  lastCommitted.clear();
}

function targetOf(event) {
  return event.target instanceof Element ? event.target : (event.target?.parentElement ?? null);
}

async function saveProvider() {
  const get = (name) => boardHost.querySelector(`[data-provider="${name}"]`)?.value ?? '';
  await send('set_provider', {
    engine: get('engine'),
    api_key: get('api_key'),
    secret: get('secret'),
    model: get('model'),
    base_url: get('base_url'),
  });
}

function saveSettings() {
  const checked = (name) =>
    boardHost.querySelector(`[data-setting="${name}"]`)?.checked ?? false;
  send('set_settings', {
    auto_read_clipboard: checked('auto_read_clipboard'),
    hover_delay_ms:
      Number(boardHost.querySelector('[data-setting="hover_delay_ms"]')?.value) || 200,
    selection_enabled: checked('selection_enabled'),
    auto_check_updates: checked('auto_check_updates'),
  });
}

// --- 事件全部委托在常驻根节点上，DOM 内部怎么替换都不影响 ---

root.addEventListener('click', (event) => {
  const button = targetOf(event)?.closest('[data-action]');
  if (!button) return;
  const { action, lang } = button.dataset;
  switch (action) {
    case 'speak':
      speakCard(lang);
      break;
    case 'copy':
      send('copy_card', { lang });
      break;
    case 'retry':
      send('retry_failed');
      break;
    case 'languages':
      languagesOpen = !languagesOpen;
      if (lastView) {
        lastBoardHtml = null;
        draw(lastView);
      }
      break;
    case 'pin':
      send('pin');
      break;
    case 'clear':
      cancelAllEdits();
      send('clear_document');
      break;
    case 'check-updates':
      send('check_updates');
      break;
    case 'open-release':
      send('open_release_page');
      break;
    case 'save-provider':
      saveProvider();
      break;
    case 'test-provider':
      send('test_provider');
      break;
    case 'close_card':
      closeCard(lang);
      break;
    case 'add_card':
      addCard();
      break;
    case 'selection-toggle':
      send('toggle_selection');
      break;
    default:
      break;
  }
});

// 改语言只重画受影响的那张卡 + 刷新其它卡的语言候选，不再整板 innerHTML 替换。
function patchCardsAfterLanguageChange(index, view, oldLang) {
  const cards = view?.cards ?? [];
  const target = cards[index];
  const els = [...boardHost.querySelectorAll('.card')];
  if (!target || !els[index]) return;

  const usedCodes = cards.map((card) => card.lang);
  els[index].outerHTML = Card({
    card: target,
    canClose: usedCodes.length > 2,
    usedCodes,
  });

  // 一张卡换了语言会改变“哪些语言还被别人占用”，其余卡的下拉候选集合随之变化；
  // 只重建下拉这一小块，不动它们的文本与状态。
  [...boardHost.querySelectorAll('.card')].forEach((cardEl, i) => {
    if (i === index) return;
    const card = cards[i];
    const wrap = cardEl.querySelector('.lang-select');
    if (!card || !wrap) return;
    wrap.outerHTML = LangAutocomplete({
      current: card.lang,
      usedCodes: usedCodes.filter((code) => code !== card.lang),
    });
  });

  lastCommitted.delete(oldLang);
  lastCommitted.set(target.lang, target.text);
  // 后续状态推送仍会整板刷新；先让缓存失效，避免用旧 html 比较跳过一次必要更新。
  lastBoardHtml = null;
}

// 卡片上方语言选择器变更
root.addEventListener('change', async (event) => {
  const sel = targetOf(event);
  if (!sel?.matches('.lang-select__select')) return;
  const newLang = sel.value;
  const oldLang = sel.dataset.lang;
  if (!newLang || newLang === oldLang) return;
  const card = sel.closest('.card');
  const index = [...boardHost.querySelectorAll('.card')].indexOf(card);
  if (index < 0) return;
  const view = await sendSilent('change_card_lang', { from: oldLang, to: newLang });
  patchCardsAfterLanguageChange(index, view, oldLang);
});

// --- 卡片拖拽排序：拖到目标卡片的左/右半区即插入对应位置，松手后顺序写回语言池 ---
let dragLang = null;

root.addEventListener('dragstart', (event) => {
  const card = targetOf(event)?.closest('.card');
  if (root.dataset.view !== 'expanded' || !card) {
    event.preventDefault();
    return;
  }
  // 从文本、下拉或按钮上起拖属于控件操作，不算排序。
  if (targetOf(event)?.closest('textarea, select, button, input, .card__close')) {
    event.preventDefault();
    return;
  }
  dragLang = card.dataset.lang;
  event.dataTransfer.effectAllowed = 'move';
  event.dataTransfer.setData('text/plain', dragLang);
  card.style.opacity = '0.45';
});

root.addEventListener('dragover', (event) => {
  if (!dragLang) return;
  const over = targetOf(event)?.closest('.card');
  if (!over) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = 'move';
  if (over.dataset.lang === dragLang) return;
  const moving = boardHost.querySelector(`.card[data-lang="${dragLang}"]`);
  const container = over.parentElement;
  if (!moving || !container) return;
  // 卡片横向排列：落在目标卡左半边插到它前面，右半边插到它后面。
  const rect = over.getBoundingClientRect();
  const before = event.clientX < rect.left + rect.width / 2;
  container.insertBefore(moving, before ? over : over.nextSibling);
});

root.addEventListener('drop', (event) => {
  if (!dragLang) return;
  event.preventDefault();
  const order = [...boardHost.querySelectorAll('.card')].map((el) => el.dataset.lang);
  const previous = lastView?.cards?.map((card) => card.lang) ?? [];
  if (order.join('|') !== previous.join('|')) {
    sendSilent('reorder_cards', { order }).then((view) => {
      if (view) lastView = view;
    });
  }
});

root.addEventListener('dragend', () => {
  if (!dragLang) return;
  const el = boardHost.querySelector(`.card[data-lang="${dragLang}"]`);
  if (el) el.style.removeProperty('opacity');
  dragLang = null;
  // 顺序已在 DOM 里落定；缓存作废，后续状态推送会按新顺序整板刷新。
  lastBoardHtml = null;
});

async function closeCard(lang) {
  const card = boardHost.querySelector(`.card[data-lang="${lang}"]`);
  if (card) {
    card.classList.add('is-removing');
    // 等动画结束再通知 Rust
    await new Promise((r) => setTimeout(r, 280));
  }
  await send('remove_card', { lang });
}

async function addCard() {
  await send('add_card');
}

root.addEventListener('input', (event) => {
  const el = targetOf(event);
  if (el?.matches('textarea.card__text')) editDebouncer.schedule(el.dataset.lang);
});

root.addEventListener('focusin', (event) => {
  if (targetOf(event)?.matches('textarea.card__text')) send('edit_start');
});

root.addEventListener('focusout', (event) => {
  const el = targetOf(event);
  if (!el?.matches('textarea.card__text')) return;
  editDebouncer.cancel(el.dataset.lang);
  commitEdit(el.dataset.lang, el);
  send('edit_end');
  send('get_state'); // 补上编辑期间被跳过的重绘
});

root.addEventListener(
  'compositionstart',
  () => {
    composing = true;
  },
  true,
);
root.addEventListener(
  'compositionend',
  (event) => {
    composing = false;
    const el = targetOf(event);
    // 组字期间到期的那次提交被 composing 挡下，这里必须补排一次，
    // 否则用户选完词就停手时，文本永远等不到同步。
    if (el?.matches('textarea.card__text')) editDebouncer.schedule(el.dataset.lang);
  },
  true,
);

root.addEventListener('keydown', (event) => {
  if (event.isComposing || composing) return; // 组词中的 Enter/Esc 属于选词
  const el = targetOf(event);
  if (!el?.matches('textarea.card__text')) return;
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    editDebouncer.cancel(el.dataset.lang);
    commitEdit(el.dataset.lang, el);
    el.blur();
  }
});

root.addEventListener('change', (event) => {
  const input = targetOf(event);
  if (input?.matches('[data-setting="autostart"]')) {
    send('set_autostart', { enabled: input.checked });
    return;
  }
  if (input?.matches('[data-setting]')) saveSettings();
});

root.addEventListener('mousedown', (event) => {
  if (event.button !== 0 || !invoke) return;
  const target = targetOf(event);
  if (!target) return;
  if (target.closest('[data-action="clear"]')) suppressCommit = true;
  const onCapsule = !!target.closest('#capsule') && root.dataset.view !== 'expanded';
  const head = target.closest('.board__head');
  const onHead = !!head && !target.closest('button, input, select, textarea, label, a');
  if (!onCapsule && !onHead) return;

  dragging = true;
  window.clearTimeout(leaveTimer);
  leaveTimer = null;
  window.clearTimeout(pendingTick);
  pendingTickKind = null;
  window.clearTimeout(dragGuard);
  dragGuard = window.setTimeout(() => {
    dragging = false;
  }, DRAG_GUARD_MS);
  // 系统级 start_dragging：OS 接管鼠标拖动，流畅不卡顿。
  // 边界约束由 Rust 侧 WindowEvent::Moved 监听实时 clamp y >= 0。
  invoke('start_drag')
    .catch(() => {})
    .finally(() => {
      dragging = false;
      window.clearTimeout(dragGuard);
      // 悬停计时被拖动打断：鼠标还停在胶囊上就补回这一次展开。
      if (lastView?.view === 'hover_pending') scheduleTick(lastView);
    });
});

root.addEventListener('mousemove', (event) => {
  if (dragging) return;
  if (root.dataset.view === 'expanded') {
    edgeArmed = false;
    return;
  }
  if (!onCapsuleTopEdge(event)) {
    // 还没长开就离开顶端 2px：撤销这次待展开（force 跳过 Rust 的光标复核）
    if (edgeArmed) {
      edgeArmed = false;
      if (root.dataset.view === 'hover_pending') {
        window.clearTimeout(pendingTick);
        pendingTickKind = null;
        send('hover_leave', { force: true });
      }
    }
    return;
  }
  if (edgeArmed) return; // 同一段贴边只发一次 enter，避免每帧一次 IPC
  edgeArmed = true;
  requestEnter();
});
root.addEventListener('pointerleave', requestLeave);

window.addEventListener('mouseup', () => {
  dragging = false;
  suppressCommit = false;
  window.clearTimeout(dragGuard);
});

document.addEventListener('keydown', (event) => {
  if (event.isComposing || composing) return;
  if (event.key === 'Escape') send('esc');
});
document.addEventListener('mousedown', (event) => {
  if (!root.contains(event.target)) send('outside_click');
});

// 后台翻译完成后由 Rust 主动推一次快照，不占用任何命令的往返。
if (listen) {
  listen('state', (event) => {
    if (event.payload) draw(event.payload);
  }).catch(() => {});
}

if (invoke) {
  send('get_state');
} else {
  draw(SNAPSHOT_VIEW);
}
