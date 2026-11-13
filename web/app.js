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
import { createPanelWidthTracker, flipOffsets, planPanelResize } from './panel-sync.js';
import {
  canStartCardDrag,
  cardReorderDuration,
  insertionSide,
  orderChanged,
  planCardReorder,
} from './card-drag.js';
import { createEditDebouncer, planEditingRefresh } from './edit-sync.js';
import { stalePasteLangs } from './paste-media.js';
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
// 失焦收起：窗口是否仍有焦点、指针是否在面板（=展开后的整个窗口）内。
// 只有「未锁定 + 失焦 + 指针在外」才在 leave_grace_ms 后复用 hover_leave 收起。
let windowFocused = true;
let pointerInsideWindow = false;
let focusCollapseTimer = null;
// 每段失焦只主动发一次 hover_leave：万一 Rust 因编辑态忽略这次离开，
// 不能每 800ms 重发一次 IPC（等 focusout / compositionend / 重新失焦再放行）。
let focusCollapseSent = false;
// 点「清空」会先让正在编辑的卡片失焦；这个窗口内的 blur 不得把旧文本再提交回去。
let suppressCommit = false;
let lastBoardHtml = null;
let lastView = null;
let languagesOpen = false;
// 历史列表是设置面板同级的内联面板：打开时卡片区隐藏，不浮在卡片编辑区上。
let historyOpen = false;
let historyItems = [];
let sendSeq = 0;
let dragging = false;
let dragGuard = null;
let currentAudio = null;
// 朗读状态：只用于按钮样式与「同卡再点=停止」，业务状态仍由 Rust 的 speech 事件驱动。
let speakingLang = null;
let speakingTicket = 0;
let speechSeq = 0;
// 各语言朗读能力（本地/在线/不可用），由 speech_capabilities 拉取后按语言缓存。
let speechCapabilities = null;
let speechHintTimer = null;
// 每张卡最后一次进入 Rust 的文本：失焦/回车对未变更内容不重复发起同步。
const lastCommitted = new Map();
// 卡片内粘贴的图片缩略图：只在前端本次运行内保留，不进 StateView、不写 settings。
// lang -> { url: objectURL, message: 内联状态文案, text: 归属文本, pending: 识别中 }
// `text` 是这张缩略图对应的那段文本：识别完成前是粘贴时的卡片文本，识别完成后是 OCR 结果。
// 文档内容一旦被替换，`text` 与快照对不上，缩略图就随之回收（见 paste-media.js）。
const pasteMedia = new Map();
// 只有中英文卡片接受图片粘贴；其他语言保持浏览器的默认粘贴行为。
const IMAGE_PASTE_LANGS = new Set(['zh', 'en']);
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

// --- 翻译历史入口 ---
// 列表只拿片段与元信息；点条目由 Rust 回填译文，前端不保存完整历史。

function redrawFromLastView() {
  if (!lastView) return;
  lastBoardHtml = null;
  draw(lastView);
}

function closeHistory() {
  if (!historyOpen) return;
  historyOpen = false;
  redrawFromLastView();
  // 关闭后把焦点还给头部的时钟按钮，键盘用户不会掉到 body 上。
  boardHost.querySelector('[data-action="history"]')?.focus();
}

async function toggleHistory() {
  if (historyOpen) {
    closeHistory();
    return;
  }
  const items = await sendSilent('list_history');
  if (historyOpen) return; // 等待期间已被别的操作打开/关闭
  historyItems = Array.isArray(items) ? items : [];
  historyOpen = true;
  languagesOpen = false; // 两个内联面板互斥，避免同时占位
  redrawFromLastView();
  requestAnimationFrame(() => {
    boardHost.querySelector('[data-action="apply-history"]')?.focus();
  });
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
// 定稿动效「种子生长」：基准 700ms ÷ 0.875 = 实际展开 800ms（实机复看后提速）。
// 基准毫秒 ÷ MOTION_SPEED = 实际毫秒，必须与 styles.css 的 --motion-speed 一致，
// 否则收起会被定时器提前截断。
// reduced-motion 下把速度拉到近似瞬时：CSS 动画已压到 0.01ms，这里不能再等完整时长。
const MOTION_REDUCED = typeof window !== 'undefined'
  && Boolean(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches);
const MOTION_SPEED = MOTION_REDUCED ? 60 : 0.875;
const motionMs = (base) => Math.round(base / MOTION_SPEED);

const ENTER_MOVE_MS = motionMs(700);   // 展开：基准 700ms
const EXIT_TIMEOUT_MS = motionMs(560); // 收起：基准 560ms，与 CSS opacity 的 delay+duration 对齐
const REVERSE_MS = motionMs(400);      // 收起途中回来：裁剪区补回满板，基准 400ms
// 下面两个只服务卡片增删补间，不随展开动效放大，否则加一张卡要等近一秒。
const CARD_STAGGER_MAX_MS = 120;
const CARD_ANIM_MS = 200;
// 展开入场：中卡先出，离中心每远一级晚 50ms 基准；收起反序、外围先走。
const CARD_BLOOM_BASE_MS = motionMs(240);
const CARD_BLOOM_STEP_MS = motionMs(50);
const CARD_FOLD_STEP_MS = motionMs(60);
// 卡片增删时的面板扩展/收缩：内容与窗口共用同一条 160ms 曲线，先对齐再提交。
const PANEL_RESIZE_MS = 160;
const PANEL_RESIZE_EASE = 'cubic-bezier(0.16, 1, 0.3, 1)';

let shape = 'capsule'; // 窗口几何形态：capsule=68×28，panel=613×300
let phase = 'hidden';  // hidden | entering | shown | closing | reversing
let phaseSeq = 0;      // 相位令牌：只有最新一次开合流程可以继续
let closeTimer = null;
let enterDoneTimer = null;
let pendingView = null; // 进入过渡期间的最新快照，过渡结束后再应用
// 面板可见期间的卡片数基线：展开那一帧由 beginExpand 写入，收起时清空。
const panelWidthTracker = createPanelWidthTracker();
// 面板窗口当前的矩形（Rust panel_frame 的缓存）：展开提交 resize 后写入，缩回时清空。
// 卡片增删补间靠它判断「窗口左缘会不会因贴边 clamp 平移」，从而决定要不要补 Δ。
let lastPanelFrame = null;
// 增删动画的互斥：动画期间到达的 state 快照只存 pendingView，结束后补一次 draw，
// 避免翻译完成的重渲染把窗口补间打断。
let panelAnimating = false;
let panelAnimToken = 0;
let panelPendingView = null;
let activePanelResize = null;

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
    applyPasteThumbs();
    applySpeechUi();
    return;
  }
  const html = renderBoard({ ...view, languagesOpen, historyOpen, historyItems });
  // html 没变就不碰 DOM
  if (html === lastBoardHtml) {
    applyPasteThumbs();
    applySpeechUi();
    return;
  }
  lastBoardHtml = html;
  boardHost.innerHTML = html;
  for (const card of view.cards ?? []) lastCommitted.set(card.lang, card.text);
  applyPasteThumbs();
  applySpeechUi();
}

/** 剔除不再属于当前文档内容的缩略图，并回收它们的 objectURL。 */
function prunePasteMedia(cards) {
  for (const lang of stalePasteLangs(pasteMedia, cards)) revokePasteMedia(lang);
}

/** 整板重绘会重建卡片 DOM：缩略图与内联状态每次从内存 Map 重新贴回。 */
function applyPasteThumbs() {
  for (const box of boardHost.querySelectorAll('.card__media')) {
    const entry = pasteMedia.get(box.dataset.lang);
    const image = box.querySelector('.card__media-thumb');
    const status = box.querySelector('.card__media-status');
    if (!entry) {
      box.hidden = true;
      if (image) image.style.backgroundImage = '';
      if (status) {
        status.textContent = '';
        status.hidden = true;
      }
      continue;
    }
    box.hidden = false;
    // 缩略图用 background 而不是 <img>：卡片文本的 XSS 断言保持不变。
    if (image && entry.url) image.style.backgroundImage = `url("${entry.url}")`;
    if (status) {
      status.textContent = entry.message ?? '';
      status.hidden = !entry.message;
    }
  }
}

/** 回收一张卡的缩略图：objectURL 必须显式释放，否则图片会一直留在内存里。 */
function revokePasteMedia(lang) {
  const entry = pasteMedia.get(lang);
  if (entry?.url) URL.revokeObjectURL(entry.url);
  pasteMedia.delete(lang);
}

/** 清空文档：文字与缩略图一起清掉，包括还在识别中的那一张。 */
function clearPasteMedia() {
  for (const lang of [...pasteMedia.keys()]) revokePasteMedia(lang);
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
  applySpeechUi();
}

// 卡片错峰：第 n 张 delay = min(n*30ms, 120ms)。只写内联延迟，
// 动画名仍是卡片自己的 card-pop-in（卡片模块的增删动画不受影响）。
function applyCardStagger() {
  const cards = Array.from(boardHost.querySelectorAll('.card'));
  const center = (cards.length - 1) / 2;
  const maxLevel = Math.max(center, 0);
  cards.forEach((card, index) => {
    // 胶囊在面板水平中点：展开时中卡先出、两侧同步扩散，不再从左到右排队。
    const level = Math.abs(index - center);
    const bloom = CARD_BLOOM_BASE_MS + Math.round(level * CARD_BLOOM_STEP_MS);
    // 收起反序：外围先消失、越靠中心越晚，视觉上朝胶囊聚拢。
    // 这个变量故意不在 enterDoneTimer 里清掉——整板重绘会重建卡片 DOM，
    // 真丢了也只是退回 0ms（同时收起），不会报错。
    const fold = Math.round((maxLevel - level) * CARD_FOLD_STEP_MS);
    card.style.setProperty('--card-delay', `${bloom}ms`);
    card.style.setProperty('--card-fold-delay', `${fold}ms`);
  });
}

// --- 卡片增删时的面板扩展 / 收缩 ---
//
// 时序契约：
// - 一帧只有一次窗口几何提交（resize_window / sync_panel_width 内部各自一次 SetWindowPos）；
// - 加卡先窗口后内容：内容钉在旧宽 → 提交新窗口 → board 补间到新宽，新卡随宽度露出；
// - 删卡先内容后窗口：先量剩余卡（FLIP First）→ 重排并反向位移 → 播放位移与收宽 → 最后缩窗；
// - 贴边（窗口左缘因 clamp 平移 Δ≠0）才在该次 resize 帧遮一帧内容；
//   非贴边全程可见，不做整板 opacity 0；
// - 收起沿用既有「内容先出、窗口后缩」，与本函数无关。

function panelBoard() {
  return boardHost.querySelector('#board');
}

// board 的内联几何（相对窗口，逻辑像素）。必须同时写 left + width 并清掉 right，
// 否则 inset:1px 会形成过约束，动画起点落不准。
function applyBoardRect(rect) {
  const board = panelBoard();
  if (!board) return;
  board.style.right = 'auto';
  board.style.bottom = 'auto';
  board.style.left = `${rect.left}px`;
  board.style.top = `${rect.top}px`;
  board.style.width = `${rect.width}px`;
  board.style.height = `${rect.height}px`;
}

function clearPanelGeometry() {
  boardHost.style.removeProperty('visibility');
  const board = panelBoard();
  if (board) {
    for (const name of ['left', 'top', 'width', 'height', 'right', 'bottom', 'transition']) {
      board.style.removeProperty(name);
    }
  }
  for (const card of boardHost.querySelectorAll('.card')) {
    card.style.removeProperty('transform');
    card.style.removeProperty('transition');
    card.style.removeProperty('will-change');
    card.classList.remove('is-entering');
  }
}

function measureCards() {
  return [...boardHost.querySelectorAll('.card')].map((card) => {
    const rect = card.getBoundingClientRect();
    return { lang: card.dataset.lang, left: rect.left, top: rect.top };
  });
}

function prefersReducedMotion() {
  return window.matchMedia?.('(prefers-reduced-motion: reduce)')?.matches ?? false;
}

function panelResizeDuration() {
  return prefersReducedMotion() ? 1 : PANEL_RESIZE_MS;
}

// 渲染前调用：上锁、记下旧窗口矩形与删卡的第一帧位置。
function startPanelResize(oldCount, newCount) {
  if (!lastPanelFrame) return null;
  panelAnimating = true;
  activePanelResize = {
    token: ++panelAnimToken,
    from: lastPanelFrame,
    direction: newCount > oldCount ? 'expand' : 'shrink',
    firstRects: newCount < oldCount ? measureCards() : null,
    previousLangs: new Set(
      [...boardHost.querySelectorAll('.card')].map((card) => card.dataset.lang),
    ),
  };
  // 补间期间不需要卡片自身的入场动画：settled 让 FLIP 的内联 transform 不被
  // card-pop-in / 展开错峰的 keyframes 覆盖。
  boardHost.dataset.cards = 'settled';
  return activePanelResize;
}

function abortPanelResize() {
  if (!panelAnimating) return;
  panelAnimToken += 1;
  panelAnimating = false;
  activePanelResize = null;
  panelPendingView = null;
  clearPanelGeometry();
}

function endPanelResize(ctx, endRect) {
  if (ctx && ctx.token !== panelAnimToken) return;
  if (endRect) applyBoardRect(endRect); // 落到标准 inset 矩形，随后清掉内联样式
  clearPanelGeometry();
  panelAnimating = false;
  activePanelResize = null;
  const latest = panelPendingView;
  panelPendingView = null;
  if (latest) draw(latest);
}

function startBoardTween(rect, duration) {
  const board = panelBoard();
  if (!board) return;
  board.style.transition = `left ${duration}ms ${PANEL_RESIZE_EASE}, width ${duration}ms ${PANEL_RESIZE_EASE}`;
  void board.getBoundingClientRect(); // 先让起点生效，下一句才是一次真实的状态变化
  applyBoardRect(rect);
}

// 删卡的 FLIP：渲染后按 lang 对比首帧位置，先反向位移，再与收宽同拍归零。
function applyFlip(firstRects, duration) {
  const offsets = flipOffsets(firstRects, measureCards());
  const moved = [];
  for (const { lang, dx, dy } of offsets) {
    const card = boardHost.querySelector(`.card[data-lang="${lang}"]`);
    if (!card) continue;
    card.style.transition = 'none';
    card.style.transform = `translate(${dx}px, ${dy}px)`;
    moved.push(card);
  }
  if (!moved.length) return;
  void moved[0].getBoundingClientRect(); // 提交反向位移
  for (const card of moved) {
    card.style.willChange = 'transform';
    card.style.transition = `transform ${duration}ms ${PANEL_RESIZE_EASE}`;
    card.style.transform = 'none';
  }
}

async function panelFrame() {
  if (!invoke) return null;
  try {
    return await invoke('panel_frame');
  } catch {
    return null;
  }
}

async function syncPanelWindow() {
  if (!invoke) return;
  await invoke('sync_panel_width').catch(() => {});
}

const waitMs = (ms) => new Promise((resolve) => window.setTimeout(resolve, ms));

// 渲染后调用：窗口先提交还是内容先补间，由 plan 决定；结束时补一次被推迟的 draw。
async function commitPanelResize(view, ctx) {
  const { token, from, direction, firstRects, previousLangs } = ctx;
  // 只读几何：与 sync_panel_width 同一套锚点 / clamp，不移动窗口。
  const to = await panelFrame();
  if (token !== panelAnimToken) return; // 被收起或新一轮增删打断
  if (!to || Math.abs(to.width - from.width) < 1) {
    endPanelResize(ctx, null); // 计数其实没变（池满等兜底）：不动窗口
    return;
  }
  const plan = planPanelResize(from, to);
  const duration = panelResizeDuration();

  if (direction === 'expand') {
    // 新卡先挂入场：它被 board 的旧宽裁住，随宽度露出时正好在播放。
    const added = (view.cards ?? []).find((card) => !previousLangs.has(card.lang));
    const addedCard = added
      ? boardHost.querySelector(`.card[data-lang="${added.lang}"]`)
      : null;
    if (addedCard) addedCard.classList.add('is-entering');
    applyBoardRect(plan.start); // 重新钉住旧宽（渲染换了 DOM，内联样式已丢失）
    if (plan.hideAtResize) boardHost.style.visibility = 'hidden';
    await syncPanelWindow(); // 窗口先长
    if (token !== panelAnimToken) return;
    lastPanelFrame = to;
    if (plan.postResizeStart) applyBoardRect(plan.postResizeStart);
    await nextFrame();
    if (token !== panelAnimToken) return;
    if (plan.hideAtResize) boardHost.style.removeProperty('visibility');
    startBoardTween(plan.end, duration);
    await waitMs(Math.max(duration, CARD_ANIM_MS) + 40);
    if (token !== panelAnimToken) return;
    endPanelResize(ctx, plan.end);
    return;
  }

  // 删卡：先内容、后窗口。剩余卡的反向位移与 board 收宽同拍播放。
  applyBoardRect(plan.start);
  applyFlip(firstRects ?? [], duration);
  startBoardTween(plan.preResizeEnd, duration);
  await waitMs(duration + 40);
  if (token !== panelAnimToken) return;
  if (plan.hideAtResize) boardHost.style.visibility = 'hidden';
  await syncPanelWindow(); // 内容已收好，最后缩窗
  if (token !== panelAnimToken) return;
  lastPanelFrame = to;
  if (plan.postResizeStart) applyBoardRect(plan.postResizeStart);
  await nextFrame();
  if (token !== panelAnimToken) return;
  if (plan.hideAtResize) boardHost.style.removeProperty('visibility');
  endPanelResize(ctx, plan.end);
}

// 展开 68×28 → 613×300（严格六步）
async function beginExpand(view) {
  const token = ++phaseSeq;
  clearCloseTimer();
  clearEnterTimer();
  pendingView = null;

  // 1) 同步写入进入态：内容全透明、宿主隐藏。
  //    同一帧就把胶囊藏起来：下一步 Rust 会左移窗口 272px，而 WebView 可能滞后一帧
  //    仍按旧坐标（left:0）绘制胶囊，会在新窗口左端闪出一个错位胶囊再消失。
  setPhase('entering');
  boardHost.dataset.visible = 'false';
  boardHost.dataset.motion = 'on';
  root.dataset.capsuleGone = '1';
  // 注意：data-cards='enter' 不能在这里设 —— CSS animation 在 visibility:hidden 上也会跑，
  // 等内容变可见时卡片动画已经跑了 ~50ms，直接从中间"蹦"出来而不是从头进场。
  // 必须延迟到 visible=true 的同一帧再触发（见下面第 3 步末尾）。
  renderContent(view, false);

  // 2) 尺寸切换一步到位：先切到 panel 形态（胶囊随之落到 272px，屏幕上不动），
  //    再让 Rust 用一次 SetWindowPos 提交「位置 + 尺寸」，没有中间几何帧
  setShape('panel');
  if (invoke) {
    await invoke('resize_window', { expanded: true }).catch(() => {});
    if (token !== phaseSeq) return;
    // 缓存展开后的窗口矩形：之后第一次增删要靠它判断窗口左缘是否贴边平移。
    lastPanelFrame = await panelFrame();
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
  // 展开动画落定后再评估：进入过程中不参与失焦收起。
  evaluateFocusCollapse();
  // 展开这一次 resize_window 已按语言池实时计数提交了宽度：把当前卡片数写成基线，
  // 否则展开后的第一次增删会被 observe 当成基线记录吞掉，窗口不跟随。
  panelWidthTracker.prime(view.cards?.length ?? 0);

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
  // 面板一旦开始收起就不该再出声（宽限到期才会到这里；ESC/点外理由 Rust 也会停）。
  if (speakingLang || currentAudio) stopSpeaking('panel_hidden');
  clearEnterTimer();
  pendingView = null;
  const token = ++phaseSeq;
  boardHost.dataset.motion = 'on';
  setPhase('closing');
  // 展开期间若发生过整板重绘，卡片 DOM 会被重建，inline 的错峰变量就丢了。
  // 在切到 closing 的同一帧重新贴一次，反序收起才不会退化成所有卡同时消失。
  applyCardStagger();
  // transitionend 为主（.board 的 opacity 走完 EXIT_TIMEOUT_MS 才结束），兜底同值：
  // 过渡被打断或元素不参与过渡时也能收回。两者必须与 CSS 里的 delay+duration 对齐。
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
  delete root.dataset.capsuleGone; // 与 instant 同一帧：胶囊立刻回到原位，且不走 220ms 淡入
  setShape('capsule');
  await nextFrame();
  await nextFrame();
  delete root.dataset.capsuleInstant;
  boardHost.removeAttribute('data-motion');
  clearBoard();
  panelWidthTracker.clear();
  lastPanelFrame = null;
}

// 兜底路径：拿不到窗口矩形（没有 Tauri 注入 / panel_frame 读取失败）时才用。
// 沿用旧的「整板淡出 → Rust resize → 淡入」，代价是一次重绘，
// 换来任何几何下都不会露出「旧内容 + 新尺寸」的中间帧。
let panelWidthSyncing = false;
async function syncPanelWidthSilent() {
  if (!invoke || panelWidthSyncing) return;
  panelWidthSyncing = true;
  boardHost.style.transition = 'opacity 60ms ease';
  boardHost.style.opacity = '0';
  await new Promise((r) => setTimeout(r, 60));
  await invoke('sync_panel_width').catch(() => {});
  lastPanelFrame = await panelFrame();
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
    evaluateFocusCollapse(); // 反向补间落定后，按当前焦点/指针再评估一次
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
  if (panelAnimating) {
    if (view.view === 'expanded') {
      // 增删补间期间到达的快照只记下，动画收尾后再补一次，避免打断窗口时序。
      lastView = view;
      panelPendingView = view;
      scheduleTick(view);
      return;
    }
    // 收起优先：中断补间，直接走「内容先出、窗口后缩」。
    abortPanelResize();
  }
  if (dragLang) {
    if (view.view === 'expanded') {
      // 拖动期间不整板重绘：节点顺序正被用户拖拽，替换 DOM 会打断 drag 并把顺序推回旧值。
      lastView = view;
      pendingDragView = view;
      scheduleTick(view);
      return;
    }
    // 面板要收起：放弃这次排序，继续走收起流程。
    abortCardDrag();
  }
  lastView = view;
  // 每次应用新快照前先回收不再属于这份文档的缩略图：内容被新截图/新文本替换、
  // 清空或收起清空文档后，旧图不能再贴到同名卡片上。非展开态也走这里，
  // 否则收起时被清空的缩略图会残留到下一次展开。
  prunePasteMedia(view.cards);
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
      // 面板开着时卡片数变了：按「窗口 / 内容」时序做扩展或收缩。
      const previousCount = panelWidthTracker.count();
      const curCount = view.cards?.length ?? 0;
      const countChanged = panelWidthTracker.observe(curCount);
      if (countChanged) {
        // 渲染前先上锁并量好删卡的第一帧位置；渲染后立刻进入补间。
        const ctx = startPanelResize(previousCount ?? curCount, curCount);
        renderContent(view, wasExpanded);
        if (ctx) {
          commitPanelResize(view, ctx).catch((error) => {
            console.warn('[bead] panel-resize', error);
            abortPanelResize();
            syncPanelWidthSilent(); // 兜底：至少保证窗口宽度跟上计数
          });
        } else {
          syncPanelWidthSilent();
        }
      } else {
        renderContent(view, wasExpanded);
      }
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
  // 视图变化会改变收起条件（翻译结束、锁定切换、面板已收起）：重新评估失焦收起。
  evaluateFocusCollapse();
}

function requestEnter() {
  if (dragging) return;
  window.clearTimeout(leaveTimer);
  leaveTimer = null;
  if (root.dataset.view === 'expanded') return;
  send('hover_enter');
}

// 展开不再由「鼠标进入窗口」触发：必须同时满足
//   1) 悬停在胶囊上；2) 光标纵坐标贴近胶囊顶端 10px 以内。
// 这样鼠标只是路过胶囊中下部（或贴边滑过）不会弹出面板。
const CAPSULE_TRIGGER_Y = 10;
let edgeArmed = false;

function onCapsuleTopEdge(event) {
  if (event.clientY >= CAPSULE_TRIGGER_Y) return false;
  const capsule = document.getElementById('capsule');
  if (!capsule) return false;
  const rect = capsule.getBoundingClientRect();
  return event.clientX >= rect.left && event.clientX <= rect.right;
}

function requestLeave() {
  if (dragging || dragLang) return; // 卡片拖拽中指针可能移出窗口，不按悬停离开处理
  window.clearTimeout(leaveTimer);
  leaveTimer = window.setTimeout(() => {
    leaveTimer = null;
    if (!dragging) send('hover_leave'); // Rust 会复核光标是否真的在窗口外
  }, LEAVE_DEBOUNCE_MS);
}

// --- 失焦收起 ---
// 用户实测缺口：卡片编辑中指针先离开（hover_leave 被编辑态拦下），随后点击别的
// 窗口，edit_end 之后没有任何计时再挂上，面板就一直开着。
// 这里只负责「什么时候发 hover_leave」，收起动作仍走 HoverStateMachine 的
// Collapsing → grace → Collapse 与既有对称补间，不新增第二条收起实现。
function focusCollapseEligible() {
  if (!invoke || dragging || dragLang) return false;
  if (windowFocused || pointerInsideWindow || composing) return false;
  const view = lastView;
  if (!view || view.view !== 'expanded' || view.locked) return false;
  if (phase !== 'shown') return false; // 展开动画未落定不参与，避免打断进入
  if (editDebouncer.hasPending()) return false;
  if ((view.cards ?? []).some((card) => card.status === 'syncing')) return false;
  return true;
}

function evaluateFocusCollapse() {
  window.clearTimeout(focusCollapseTimer);
  focusCollapseTimer = null;
  if (focusCollapseSent || !focusCollapseEligible()) return;
  const delay = lastView.settings?.leave_grace_ms ?? DEFAULT_LEAVE_GRACE_MS;
  focusCollapseTimer = window.setTimeout(() => {
    focusCollapseTimer = null;
    // 宽限期内状态可能又变了（重新组字 / 开始翻译 / 指针回来）：
    // 下一次 draw 或对应事件会重新评估，这里不硬收。
    if (focusCollapseSent || !focusCollapseEligible()) return;
    focusCollapseSent = true;
    send('hover_leave');
  }, delay);
}

// 朗读：本地引擎自己发声；在线产物走 asset 协议。失败必须可见，不再静默吞掉。
function showSpeechHint(message) {
  if (!message) return;
  let hint = document.getElementById('speech-hint');
  if (!hint) {
    hint = document.createElement('div');
    hint.id = 'speech-hint';
    hint.className = 'speech-hint';
    document.body.appendChild(hint);
  }
  hint.textContent = message;
  hint.dataset.visible = 'true';
  window.clearTimeout(speechHintTimer);
  speechHintTimer = window.setTimeout(() => {
    hint.dataset.visible = 'false';
  }, 2600);
}

function applySpeechUi() {
  for (const card of boardHost.querySelectorAll('.card')) {
    const lang = card.dataset.lang;
    const capability = speechCapabilities?.[lang];
    const button = card.querySelector('[data-action="speak"]');
    if (!button) continue;
    const playing = speakingLang === lang;
    button.classList.toggle('is-speaking', playing);
    button.classList.toggle('is-online', !!capability && capability.backend === 'online');
    button.disabled = !!capability && !capability.enabled;
    button.title = playing ? '停止朗读' : capability?.reason || '朗读';
    button.setAttribute('aria-pressed', playing ? 'true' : 'false');
  }
}

async function refreshSpeechCapabilities() {
  if (!invoke) return;
  try {
    const data = await invoke('speech_capabilities');
    if (!data?.languages) return;
    speechCapabilities = Object.fromEntries(data.languages.map((item) => [item.lang, item]));
    applySpeechUi();
  } catch (error) {
    console.warn('[bead] speech_capabilities', error);
  }
}

function pauseSpeechAudio() {
  if (!currentAudio) return;
  currentAudio.onended = null;
  currentAudio.onerror = null;
  currentAudio.pause();
}

function playSpeechFile(result) {
  if (!convertFileSrc) {
    showSpeechHint('当前环境无法播放在线朗读音频');
    return;
  }
  let url;
  try {
    url = convertFileSrc(result.url);
  } catch (error) {
    showSpeechHint(`朗读文件不可用：${error}`);
    return;
  }
  if (!currentAudio) currentAudio = new Audio();
  currentAudio.pause();
  currentAudio.src = url; // 换 src 后自然从头播；复用同一个元素，避免每点一次就泄漏一个 Audio
  const settings = lastView?.settings ?? {};
  const volume = Number(settings.tts_volume ?? 80);
  const rate = Number(settings.tts_rate ?? 1);
  currentAudio.volume = Math.min(1, Math.max(0, volume / 100));
  currentAudio.playbackRate = Math.min(2, Math.max(0.5, rate));
  currentAudio.onended = () => {
    currentAudio.onended = null;
    currentAudio.onerror = null;
    if (invoke && result.ticket === speakingTicket) {
      invoke('speech_finished', { ticket: result.ticket }).catch(() => {});
    }
    speakingLang = null;
    applySpeechUi();
  };
  currentAudio.onerror = () => {
    currentAudio.onerror = null;
    showSpeechHint('在线朗读音频播放失败');
    if (invoke) {
      invoke('speech_failed', {
        ticket: result.ticket,
        lang: result.lang,
        message: '在线朗读音频播放失败',
      }).catch(() => {});
    }
    speakingLang = null;
    applySpeechUi();
  };
  currentAudio.play().catch((error) => {
    showSpeechHint(`在线朗读音频播放失败：${error}`);
    if (invoke) {
      invoke('speech_failed', {
        ticket: result.ticket,
        lang: result.lang,
        message: `在线朗读音频播放失败：${error}`,
      }).catch(() => {});
    }
    speakingLang = null;
    applySpeechUi();
  });
}

function stopSpeaking(reason = 'user') {
  pauseSpeechAudio();
  speakingLang = null;
  applySpeechUi();
  if (invoke) invoke('stop_speaking', { reason }).catch(() => {});
}

function applySpeechEvent(view) {
  if (!view || typeof view !== 'object') return;
  const phase = view.phase;
  if (phase === 'resolving' || phase === 'playing') {
    // 旧请求的 stopped 可能排在 resolving 之前；ticket 只许前进，防旧事件清掉新状态。
    if (view.ticket >= speakingTicket) {
      speakingLang = view.lang || null;
      speakingTicket = view.ticket;
    }
  } else if (phase === 'finished' || phase === 'stopped' || phase === 'idle' || phase === 'error') {
    if (view.ticket >= speakingTicket) {
      speakingLang = null;
      speakingTicket = view.ticket;
      pauseSpeechAudio();
    }
    if (phase === 'error' && view.error) showSpeechHint(view.error);
  }
  applySpeechUi();
}

async function speakCard(lang) {
  if (!invoke) return;
  if (speakingLang === lang) {
    stopSpeaking('user');
    return;
  }
  // 编辑中的文本尚未进 Rust：先 flush，否则会读到旧内容（朗读与卡片文本不一致）。
  const textarea = [...boardHost.querySelectorAll('textarea.card__text')].find(
    (element) => element.dataset.lang === lang,
  );
  if (textarea) {
    editDebouncer.cancel(lang);
    await commitEdit(lang, textarea);
  }
  const seq = ++speechSeq;
  let result;
  try {
    result = await invoke('speak_card', { lang });
  } catch (error) {
    if (seq === speechSeq) showSpeechHint(`朗读请求失败：${error}`);
    return;
  }
  if (seq !== speechSeq || !result) return;
  if (result.kind === 'cancelled') return;
  if (result.kind === 'error' || result.kind === 'unavailable' || result.kind === 'empty') {
    showSpeechHint(result.error || '当前无法朗读');
    refreshSpeechCapabilities();
    return;
  }
  speakingLang = result.lang;
  speakingTicket = result.ticket;
  applySpeechUi();
  if (result.kind === 'file') playSpeechFile(result);
}

async function commitEdit(lang, textarea) {
  if (root.dataset.view !== 'expanded' || !boardHost.contains(textarea)) return;
  if (composing || suppressCommit) return;
  const text = textarea.value;
  if (lastCommitted.get(lang) === text) return;
  lastCommitted.set(lang, text);
  await send('commit_edit', { lang, text });
}

// 卡片内粘贴图片：OCR 只按这张卡的语言建引擎，结果直接提交为这张卡的权威源。
// 失败/无文字只写缩略图区域的内联状态（项目已无 notice，不弹全局提示）。
async function ocrPasteIntoCard(lang, entry) {
  if (!invoke) return;
  let text;
  try {
    text = await invoke('ocr_clipboard_image_for_lang', { lang });
  } catch (error) {
    console.warn('[bead] paste-ocr', error);
    if (pasteMedia.get(lang) === entry) {
      entry.message = '识别失败';
      entry.pending = false;
      applyPasteThumbs();
    }
    return;
  }
  if (pasteMedia.get(lang) !== entry) return; // 期间又粘了新图，旧结果作废
  if (typeof text !== 'string' || !text.trim()) {
    entry.message = '未识别到文字';
    entry.pending = false;
    applyPasteThumbs();
    return;
  }
  entry.message = '';
  applyPasteThumbs();
  // 该卡成为权威源；其余卡由 Rust 的 commit 流程负责翻译，不再 auto-detect 换卡。
  lastCommitted.set(lang, text);
  const textarea = [...boardHost.querySelectorAll('textarea.card__text')].find(
    (element) => element.dataset.lang === lang,
  );
  if (textarea) textarea.value = text;
  await send('commit_edit', { lang, text });
  // 缩略图从此归属这段 OCR 文本：下一次快照里文本一旦不同（新截图/新文本/清空），
  // draw() 里的 prunePasteMedia 就会把它回收。若提交期间已被回收则不再复活。
  if (pasteMedia.get(lang) === entry) {
    entry.text = text;
    entry.pending = false;
  }
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

async function saveSettings() {
  const checked = (name, fallback = false) =>
    boardHost.querySelector(`[data-setting="${name}"]`)?.checked ?? fallback;
  const value = (name) =>
    Number(boardHost.querySelector(`[data-setting="${name}"]`)?.value);
  const view = await send('set_settings', {
    auto_read_clipboard: checked('auto_read_clipboard'),
    hover_delay_ms:
      Number(boardHost.querySelector('[data-setting="hover_delay_ms"]')?.value) || 200,
    selection_enabled: checked('selection_enabled'),
    selection_anchor_enabled: checked('selection_anchor_enabled'),
    selection_keyboard_fallback: checked('selection_keyboard_fallback'),
    selection_hotkey:
      boardHost.querySelector('[data-setting="selection_hotkey"]')?.value ?? '',
    auto_check_updates: checked('auto_check_updates'),
    tts_rate: value('tts_rate') || 1,
    tts_volume: Number.isFinite(value('tts_volume')) ? value('tts_volume') : 80,
    tts_online_fallback: checked('tts_online_fallback', true),
  });
  await refreshSpeechCapabilities();
  return view;
}

// --- 事件全部委托在常驻根节点上，DOM 内部怎么替换都不影响 ---

root.addEventListener('click', (event) => {
  const button = targetOf(event)?.closest('[data-action]');
  if (!button) return;
  const { action, lang } = button.dataset;
  switch (action) {
    case 'dismiss-paste': {
      // 只关缩略图，不动已识别/已输入的文字，也不发任何请求。
      const mediaLang = button.closest('.card__media')?.dataset.lang;
      if (mediaLang) revokePasteMedia(mediaLang);
      applyPasteThumbs();
      break;
    }
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
      historyOpen = false;
      languagesOpen = !languagesOpen;
      if (lastView) {
        lastBoardHtml = null;
        draw(lastView);
      }
      break;
    case 'history':
      toggleHistory();
      break;
    case 'apply-history': {
      const id = Number(button.dataset.id);
      // 先关列表再发请求：条目点击即回填并关闭，不等翻译往返。
      historyOpen = false;
      historyItems = [];
      redrawFromLastView();
      if (Number.isFinite(id)) send('apply_history', { id });
      break;
    }
    case 'clear-history':
      historyItems = [];
      send('clear_history');
      break;
    case 'pin':
      send('pin');
      break;
    case 'clear':
      cancelAllEdits();
      pauseSpeechAudio();
      speakingLang = null;
      applySpeechUi();
      clearPasteMedia(); // 文字与缩略图必须一起清掉
      send('clear_document');
      break;
    case 'check-updates':
      send('check_updates');
      break;
    case 'ocr-install':
      send('ocr_install');
      break;
    case 'ocr-remove':
      send('ocr_remove');
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
  refreshSpeechCapabilities();
});

// --- 卡片拖拽排序 ---
//
// 只用 HTML5 拖拽重排，不碰窗口：start_drag 只由胶囊 / .board__head 的 mousedown 触发，
// 卡片上的拖拽不会走进那条链路，Moved 与收起锚点逻辑都不参与。
// 拖动中每次跨越目标卡只做「重排 DOM + FLIP」，松手才写回 Rust 的 reorder_cards；
// 拖回原位不产生 IPC。源卡留在落位槽里当占位（虚线 + 半透明），拖影由系统跟随。
let dragLang = null;
let dragOriginOrder = [];
let dragDropped = false;
let pendingDragView = null;
let dragStyleTimer = null;

function cardElements() {
  return [...boardHost.querySelectorAll('.card')];
}

function isEditingCard() {
  return composing || document.activeElement?.matches?.('textarea.card__text') === true;
}

function setDropTarget(card, side) {
  for (const el of cardElements()) {
    el.classList.toggle('is-drop-before', el === card && side === 'before');
    el.classList.toggle('is-drop-after', el === card && side === 'after');
  }
}

function clearDropTargets() {
  for (const el of cardElements()) el.classList.remove('is-drop-before', 'is-drop-after');
}

// 落点在卡片之间的空隙时，取水平距离最近的那张当落点。
function nearestCard(clientX) {
  let best = null;
  let bestDistance = Infinity;
  for (const card of cardElements()) {
    const rect = card.getBoundingClientRect();
    const distance =
      clientX < rect.left ? rect.left - clientX : clientX > rect.right ? clientX - rect.right : 0;
    if (distance < bestDistance) {
      best = card;
      bestDistance = distance;
    }
  }
  return best;
}

// 按目标顺序重排现有节点（appendChild 只移动、不重建）：正在拖的卡与拖影都不会断。
// 被拖的那张不参与 FLIP —— 它的视觉由系统拖影负责，源卡本身留在落位槽里当占位。
function reorderDom(order, duration, excludeLang) {
  const container = boardHost.querySelector('.board__cards');
  if (!container) return;
  const before = measureCards().filter((rect) => rect.lang !== excludeLang);
  for (const lang of order) {
    const el = container.querySelector(`.card[data-lang="${lang}"]`);
    if (el) container.appendChild(el);
  }
  if (duration === 0) return; // reduced-motion：直接落位
  const after = measureCards().filter((rect) => rect.lang !== excludeLang);
  applyReorderFlip(before, after, duration);
}

function applyReorderFlip(beforeRects, afterRects, duration) {
  const offsets = flipOffsets(beforeRects, afterRects);
  const moved = [];
  for (const { lang, dx, dy } of offsets) {
    const card = boardHost.querySelector(`.card[data-lang="${lang}"]`);
    if (!card) continue;
    card.style.transition = 'none';
    card.style.transform = `translate(${dx}px, ${dy}px)`;
    moved.push(card);
  }
  if (!moved.length) return;
  void moved[0].getBoundingClientRect(); // 提交反向位移
  for (const card of moved) {
    card.style.willChange = 'transform';
    card.style.transition = `transform ${duration}ms ${PANEL_RESIZE_EASE}`;
    card.style.transform = 'none';
  }
}

function clearReorderStyles() {
  if (dragStyleTimer) {
    window.clearTimeout(dragStyleTimer);
    dragStyleTimer = null;
  }
  clearDropTargets();
  for (const card of cardElements()) {
    card.classList.remove('is-dragging');
    card.style.removeProperty('transition');
    card.style.removeProperty('transform');
    card.style.removeProperty('will-change');
  }
}

function scheduleReorderStyleCleanup(duration) {
  if (dragStyleTimer) window.clearTimeout(dragStyleTimer);
  dragStyleTimer = window.setTimeout(() => {
    dragStyleTimer = null;
    clearReorderStyles();
  }, duration + 40);
}

// 收起或状态被强制打断：放弃这次排序（DOM 会被下一次整板刷新覆盖），不发 IPC。
function abortCardDrag() {
  dragLang = null;
  dragOriginOrder = [];
  dragDropped = false;
  pendingDragView = null;
  clearReorderStyles();
}

root.addEventListener('dragstart', (event) => {
  const target = targetOf(event);
  const grip = target?.closest('.card__grip');
  const card = target?.closest('.card');
  const allowed =
    !!card &&
    !!grip &&
    canStartCardDrag({
      expanded: root.dataset.view === 'expanded',
      editing: isEditingCard(),
      onGrip: true,
      // 展开 / 增删补间期间卡片自带 keyframes，FLIP 的 transform 会被覆盖。
      busy:
        phase !== 'shown' || panelAnimating || boardHost.dataset.cards !== 'settled',
    });
  if (!allowed) {
    event.preventDefault();
    return;
  }
  dragLang = card.dataset.lang;
  dragOriginOrder = cardElements().map((el) => el.dataset.lang);
  dragDropped = false;
  pendingDragView = null;
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', dragLang);
    // 从把手起拖，但拖影用整张卡；锚点按按下处相对卡片的偏移换算。
    const rect = card.getBoundingClientRect();
    event.dataTransfer.setDragImage(
      card,
      Math.round(event.clientX - rect.left),
      Math.round(event.clientY - rect.top),
    );
  }
  card.classList.add('is-dragging');
  // 拖拽期间不按悬停离开收起：指针可能移出窗口，但这次操作归用户拖拽。
  window.clearTimeout(leaveTimer);
  leaveTimer = null;
});

root.addEventListener('dragover', (event) => {
  if (!dragLang) return;
  const target = targetOf(event);
  let over = target?.closest('.card') ?? null;
  if (!over) {
    // 落在卡片之间的空隙：仍允许放置，按最近的一张算落点。
    if (!target?.closest('.board__cards')) return;
    over = nearestCard(event.clientX);
    if (!over) return;
  }
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  const moving = boardHost.querySelector(`.card[data-lang="${dragLang}"]`);
  if (!moving) return;
  if (over === moving) {
    clearDropTargets();
    return;
  }
  const side = insertionSide(event.clientX, over.getBoundingClientRect());
  setDropTarget(over, side);
  const current = cardElements().map((el) => el.dataset.lang);
  const next = planCardReorder(current, dragLang, over.dataset.lang, side);
  if (!orderChanged(current, next)) return;
  reorderDom(next, cardReorderDuration(prefersReducedMotion()), dragLang);
});

root.addEventListener('drop', (event) => {
  if (!dragLang) return;
  event.preventDefault(); // 标记「在面板内松手」，真正的提交放在 dragend
  dragDropped = true;
});

root.addEventListener('dragend', () => {
  if (!dragLang) return;
  const lang = dragLang;
  const order = cardElements().map((el) => el.dataset.lang);
  const dropped = dragDropped;
  dragDropped = false;
  dragLang = null; // 先解锁，下面 draw 才会真正应用快照
  const duration = cardReorderDuration(prefersReducedMotion());
  clearDropTargets();

  if (!dropped) {
    // 拖到面板外松手 = 取消：剩余卡按 FLIP 回到起拖时的顺序。
    if (orderChanged(dragOriginOrder, order)) reorderDom(dragOriginOrder, duration, lang);
    dragOriginOrder = [];
    if (pendingDragView) {
      const latest = pendingDragView;
      pendingDragView = null;
      draw(latest);
    }
    scheduleReorderStyleCleanup(duration);
    return;
  }

  const changed = orderChanged(dragOriginOrder, order);
  dragOriginOrder = [];
  if (changed) {
    sendSilent('reorder_cards', { order }).then((view) => {
      if (!view) return;
      lastView = view;
      // 整板对齐 Rust 的新顺序；重绘顺带清掉 FLIP 的内联样式。
      draw(view);
    });
  } else if (pendingDragView) {
    const latest = pendingDragView;
    pendingDragView = null;
    draw(latest);
  }
  scheduleReorderStyleCleanup(duration);
});

async function closeCard(lang) {
  const card = boardHost.querySelector(`.card[data-lang="${lang}"]`);
  if (card) {
    card.classList.add('is-removing');
    // 等动画结束再通知 Rust
    await new Promise((r) => setTimeout(r, 280));
  }
  await send('remove_card', { lang });
  refreshSpeechCapabilities();
}

async function addCard() {
  await send('add_card');
  refreshSpeechCapabilities();
}

root.addEventListener('input', (event) => {
  const el = targetOf(event);
  if (el?.matches('textarea.card__text')) editDebouncer.schedule(el.dataset.lang);
});

// 卡片内粘贴图片：只在中英文卡片上接管，且仅在剪贴板确实带图片时接管。
// 有文字 + 图片时以图片为准；多张只取最先命中的一张；其他语言完全不拦默认粘贴。
root.addEventListener('paste', (event) => {
  const el = targetOf(event);
  if (!el?.matches('textarea.card__text')) return;
  const lang = el.dataset.lang;
  if (!IMAGE_PASTE_LANGS.has(lang)) return;
  const imageItem = [...(event.clipboardData?.items ?? [])].find(
    (item) => item.kind === 'file' && item.type.startsWith('image/'),
  );
  if (!imageItem) return;
  event.preventDefault();
  const file = imageItem.getAsFile();
  if (!file) return;
  const previous = pasteMedia.get(lang);
  if (previous?.url) URL.revokeObjectURL(previous.url);
  // 归属文本取自当前快照：识别完成前它就是这张缩略图对应的内容，识别失败时也用它判定过期。
  const baseline = lastView?.cards?.find((card) => card.lang === lang)?.text ?? '';
  const entry = { url: URL.createObjectURL(file), message: '识别中…', text: baseline, pending: true };
  pasteMedia.set(lang, entry);
  applyPasteThumbs();
  ocrPasteIntoCard(lang, entry).catch(() => {});
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
  // 编辑态会拦下 hover_leave；失焦后重新评估，让「先移出、后切走窗口」也能收起。
  focusCollapseSent = false;
  evaluateFocusCollapse();
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
    // 组字期间禁止的收起，结束之后按当前焦点/指针状态重新评估。
    focusCollapseSent = false;
    evaluateFocusCollapse();
  },
  true,
);

root.addEventListener('keydown', (event) => {
  if (event.isComposing || composing) return; // 组词中的 Enter/Esc 属于选词
  if (historyOpen) {
    if (event.key === 'Escape') {
      // 截住 Esc：否则 document 层会把它当成「收起面板」。
      event.preventDefault();
      event.stopPropagation();
      closeHistory();
      return;
    }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      const items = [...boardHost.querySelectorAll('[data-action="apply-history"]')];
      if (!items.length) return;
      event.preventDefault();
      const current = items.indexOf(document.activeElement);
      const step = event.key === 'ArrowDown' ? 1 : -1;
      const next =
        current < 0
          ? step > 0
            ? 0
            : items.length - 1
          : Math.max(0, Math.min(items.length - 1, current + step));
      items[next]?.focus();
      return;
    }
  }
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
  if (input?.matches('[data-ocr-engine]')) {
    send('set_ocr_engine', { engine: input.dataset.ocrEngine });
    return;
  }
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
    // 还没长开就离开顶端 10px：撤销这次待展开（force 跳过 Rust 的光标复核）
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
root.addEventListener('pointerenter', () => {
  // 指针回到面板：取消待收起的失焦收起。
  pointerInsideWindow = true;
  focusCollapseSent = false;
  evaluateFocusCollapse();
});
root.addEventListener('pointerleave', () => {
  pointerInsideWindow = false;
  // 鼠标离开仍走原有 200ms 防抖；失焦收起只在 window blur / 编辑结束 / 视图变化时评估。
  requestLeave();
});

// 失焦收起：窗口失去焦点时启动；重新获得焦点时取消。
window.addEventListener('blur', () => {
  windowFocused = false;
  focusCollapseSent = false; // 新的一段失焦，允许再次尝试
  evaluateFocusCollapse();
});
window.addEventListener('focus', () => {
  windowFocused = true;
  focusCollapseSent = false;
  evaluateFocusCollapse();
});

window.addEventListener('mouseup', () => {
  dragging = false;
  suppressCommit = false;
  window.clearTimeout(dragGuard);
});

document.addEventListener('keydown', (event) => {
  if (event.isComposing || composing) return;
  if (event.key === 'Escape' && historyOpen) {
    closeHistory();
    return;
  }
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
  // 下载进度单独走事件：只更新进度条与状态文案，不重绘面板，避免打断滚动与焦点。
  listen('ocr-progress', (event) => {
    const payload = event.payload;
    if (!payload) return;
    const bar = boardHost.querySelector('[data-ocr-progress]');
    if (bar) {
      bar.hidden = false;
      bar.max = payload.total || 1;
      bar.value = payload.overall || 0;
    }
    if (payload.phase === 'done') return;
    const status = boardHost.querySelector('[data-ocr-status]');
    if (!status) return;
    const percent = payload.total ? Math.floor((payload.overall / payload.total) * 100) : 0;
    const phase = payload.phase === 'verifying' ? '校验' : '下载';
    status.textContent = `${phase} ${payload.file} · ${percent}%`;
  }).catch(() => {});
}

if (invoke) {
  send('get_state');
  refreshSpeechCapabilities();
} else {
  draw(SNAPSHOT_VIEW);
}
