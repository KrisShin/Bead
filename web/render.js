// 纯展示层：输入 Rust 给的 state 快照，输出 HTML 字符串。
// 组件全部在 components/ 目录中定义，这里只做组装。

import { escapeHtml } from './components/escape.js';
import { icon } from './components/icons.js';
import {
  Capsule,
  HeadButton,
  Card,
  AddCardButton,
  langName,
} from './components/index.js';

// 只保留传统机器翻译引擎；大模型翻译（智谱 / 硅基流动 / 自定义 OpenAI）暂不开放。
const ENGINES = [
  ['zerokey', '默认（免费）'],
  ['deepl', 'DeepL Free'],
  ['tencent', '腾讯云'],
  ['volc', '火山引擎'],
  ['aliyun', '阿里云'],
  ['baidu', '百度翻译'],
  ['youdao', '有道'],
];

const EMPTY_PROVIDER = {
  engine: 'zerokey',
  has_key: false,
  has_secret: false,
  needs_secret: false,
  model: '',
  base_url: '',
};

export { escapeHtml };

/** 历史时间戳 → 本地 MM-DD HH:mm；无效时间不显示。 */
function historyTime(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '';
  const date = new Date(ms);
  const pad = (value) => String(value).padStart(2, '0');
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 字节数 → 人类可读体积，用于设置页展示需要下载的大小。 */
function formatBytes(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * OCR 能力区块：引擎选择、需要下载的体积、内联进度与安装/删除。
 * 失败只在这里显示，不弹提示条；进度条由 app.js 订阅 `ocr-progress` 原地更新。
 */
function renderOcrSettings(settings) {
  const supported = settings.ocr_bundled_supported !== false;
  const installed = !!settings.ocr_installed;
  const busy = !!settings.ocr_busy;
  const selected = settings.ocr_engine === 'bundled' ? 'bundled' : 'system';
  const size = formatBytes(settings.ocr_total_size ?? 0);
  let status = `未安装，需要下载 ${size}。安装后仍需选择「内置」才会启用。`;
  if (!supported) {
    status = '当前平台仅支持系统 OCR。';
  } else if (installed) {
    const effective =
      settings.ocr_effective_engine === 'bundled' ? '内置 PP-OCRv4' : '系统 OCR';
    status = `已安装 v${escapeHtml(settings.ocr_version ?? '')}（${size}），当前使用${effective}。`;
  }
  // 推荐项在前：内置模型实测识别更稳；系统 OCR 标红并写明「体验不佳」，
  // 颜色之外还有「推荐」徽标与「✓ 已选」文字，色弱用户同样可辨。
  return `<span class="board__label">OCR 引擎</span>
    <label class="ocr-option ocr-option--recommended">
      <input type="radio" name="ocr-engine" data-ocr-engine="bundled"${selected === 'bundled' ? ' checked' : ''}${supported && installed ? '' : ' disabled'} />
      <span class="ocr-option__icon">${icon('ocr_builtin', { size: 16 })}</span>
      <span class="ocr-option__body">
        <span class="ocr-option__title">内置 PP-OCRv4<span class="ocr-option__badge">推荐</span></span>
        <span class="ocr-option__note">自带插件、离线识别，准确率与稳定性更好${installed ? '' : '；需先下载模型'}</span>
      </span>
      <span class="ocr-option__check" aria-hidden="true">✓ 已选</span>
    </label>
    <label class="ocr-option ocr-option--warn">
      <input type="radio" name="ocr-engine" data-ocr-engine="system"${selected === 'system' ? ' checked' : ''} />
      <span class="ocr-option__icon">${icon('ocr_system', { size: 16 })}</span>
      <span class="ocr-option__body">
        <span class="ocr-option__title">系统 OCR<span class="ocr-option__tag">体验不佳</span></span>
        <span class="ocr-option__note">无需下载，但识别率与稳定性较差，建议改用内置方案</span>
      </span>
      <span class="ocr-option__check" aria-hidden="true">✓ 已选</span>
    </label>
    <span class="board__hint" data-ocr-status>${status}</span>
    ${settings.ocr_error ? `<span class="board__hint board__hint--error">${escapeHtml(settings.ocr_error)}</span>` : ''}
    <progress class="board__progress" data-ocr-progress max="${Math.max(settings.ocr_total_size ?? 0, 1)}" value="0"${busy ? '' : ' hidden'}></progress>
    <span class="board__actions">
      <button type="button" data-action="ocr-install"${!supported || busy ? ' disabled' : ''}>${installed ? '重新安装' : `下载 ${size}`}</button>
      <button type="button" data-action="ocr-remove"${installed && !busy ? '' : ' disabled'}>删除</button>
    </span>`;
}

export function renderBoard(view) {
  if (view.view !== 'expanded') return '';

  const cardCount = view.cards.length;
  const canClose = cardCount > 2;
  const canAdd = cardCount < 5;
  const usedCodes = view.cards.map((c) => c.lang);

  const cards = view.cards
    .map((card) => Card({ card, canClose, usedCodes }))
    .join('');

  // 设置面板：语言完全由卡片自己控制，设置里不再出现语言选项。
  const provider = view.provider ?? EMPTY_PROVIDER;
  const providerPanel = view.languagesOpen
    ? `<div class="board__provider">
    <select data-provider="engine">${ENGINES.map(
        ([code, label]) =>
          `<option value="${code}"${provider.engine === code ? ' selected' : ''}>${label}</option>`,
      ).join('')}</select>
    <input type="password" data-provider="api_key" placeholder="${provider.has_key ? '已保存（留空保持）' : '密钥'}" />
    <input type="password" data-provider="secret" placeholder="${provider.has_secret ? '已保存（留空保持）' : '第二密钥（腾讯/火山/阿里/百度/有道）'}" />
    <button type="button" data-action="save-provider">保存</button>
    <button type="button" data-action="test-provider">测试连接</button>
  </div>`
    : '';

  const settingsSection =
    view.languagesOpen && view.settings
      ? `<div class="board__settings">
    <label><input type="checkbox" data-setting="auto_read_clipboard"${view.settings.auto_read_clipboard ? ' checked' : ''} />自动读剪贴板</label>
    <label>悬停 <input type="number" data-setting="hover_delay_ms" value="${view.settings.hover_delay_ms}" min="50" max="2000" step="50" /> ms 后展开</label>
    <label><input type="checkbox" data-setting="selection_enabled"${view.settings.selection_enabled ? ' checked' : ''} />划词（快捷键触发取词，开启后划词即显示翻译图标）</label>
    <label>快捷键 <input type="text" data-setting="selection_hotkey" value="${escapeHtml(view.settings.selection_hotkey ?? '')}" spellcheck="false" autocomplete="off" /></label>
    ${view.settings.selection_hotkey_error ? `<span class="board__hint">${escapeHtml(view.settings.selection_hotkey_error)}</span>` : ''}
    ${view.settings.selection_watch_error ? `<span class="board__hint board__hint--error">${escapeHtml(view.settings.selection_watch_error)}</span>` : ''}
    ${view.settings.selection_anchor_supported === false ? '<span class="board__hint">当前平台不支持划词图标，快捷键取词仍可用。</span>' : ''}
    <label><input type="checkbox" data-setting="autostart" ${view.settings.autostart_enabled ? 'checked' : ''} />开机自启</label>
    <span class="board__hint">有本地音色时不联网；仅在该语言缺少音色时才把卡片文本发送给在线朗读服务。</span>
    ${renderOcrSettings(view.settings)}
    <button type="button" data-action="clear-history">清空历史</button>
    <button type="button" data-action="check-updates">检查更新</button>
  </div>`
      : '';

  // 历史列表：与设置面板同一层的内联面板，打开时卡片区隐藏，不浮在卡片上。
  const historyItems = view.historyItems ?? [];
  const historyPanel = view.historyOpen
    ? `<div class="board__history" role="region" aria-label="翻译历史">
    <div class="history__head">
      <span class="history__title">翻译历史 · 最近 10 条</span>
      <button type="button" data-action="clear-history">清空历史</button>
    </div>
    ${
      historyItems.length
        ? `<ul class="history__list">${historyItems
            .map((item) => {
              const langs = (item.targets ?? []).map((code) => langName(code)).join('、');
              const time = historyTime(item.created_at_ms);
              const meta = [langs, time].filter(Boolean).join(' · ');
              return `<li><button type="button" class="history__item" data-action="apply-history" data-id="${item.id}">
        <span class="history__text">${escapeHtml(item.snippet ?? '')}</span>
        <span class="history__meta">${escapeHtml(meta)}</span>
      </button></li>`;
            })
            .join('')}</ul>`
        : '<p class="history__empty">还没有翻译历史；成功翻译后会自动记在这里。</p>'
    }
  </div>`
    : '';

  // 面板头：标题 + icon 按钮组 + 可选添加按钮
  const addBtn = canAdd ? AddCardButton() : '';
  const pinLabel = view.locked ? '已锁定（点击解锁）' : '锁定';
  const selectionOn = view.settings?.selection_enabled;

  return `<section id="board" class="board" data-card-count="${cardCount}">
  <header class="board__head">
    <span class="board__title">Bead</span>
    ${addBtn}
    <div class="board__actions">
      ${HeadButton({
        action: 'selection-toggle',
        iconName: selectionOn ? 'selection' : 'selection_off',
        label: selectionOn ? '划词翻译已开启（点击关闭）' : '划词翻译已关闭（点击开启）',
        ariaLabel: '划词翻译',
        pressed: Boolean(selectionOn),
        className: 'icon-btn--selection',
      })}
      ${HeadButton({
        action: 'history',
        iconName: 'history',
        title: '翻译历史',
        ariaLabel: '翻译历史',
        label: view.historyOpen ? '关闭翻译历史' : '翻译历史',
        pressed: Boolean(view.historyOpen),
        className: 'icon-btn--history',
      })}
      ${view.update_latest ? `<button type="button" class="icon-btn" data-action="open-release" title="下载新版 ${escapeHtml(view.update_latest)}">${escapeHtml(view.update_latest)}</button>` : ''}
      ${HeadButton({
        action: 'languages',
        iconName: 'settings',
        title: '设置',
        label: view.languagesOpen ? '关闭设置' : '设置',
        ariaLabel: '设置',
        pressed: Boolean(view.languagesOpen),
        className: 'icon-btn--settings',
      })}
      ${HeadButton({
        action: 'pin',
        iconName: view.locked ? 'lock' : 'unlock',
        label: pinLabel,
        ariaLabel: '锁定',
        pressed: Boolean(view.locked),
        className: 'icon-btn--lock',
      })}
      ${HeadButton({ action: 'clear', iconName: 'clear', label: '清空' })}
    </div>
  </header>
  ${providerPanel}
  ${settingsSection}
  ${historyPanel}
  ${view.languagesOpen || view.historyOpen ? '' : `<div class="board__cards">${cards}</div>`}
</section>`;
}
