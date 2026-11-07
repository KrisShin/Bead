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
    <label><input type="checkbox" data-setting="selection_enabled"${view.settings.selection_enabled ? ' checked' : ''} />划词（快捷键触发取词）</label>
    <label>快捷键 <input type="text" data-setting="selection_hotkey" value="${escapeHtml(view.settings.selection_hotkey ?? '')}" spellcheck="false" autocomplete="off" /></label>
    ${view.settings.selection_hotkey_error ? `<span class="board__hint">${escapeHtml(view.settings.selection_hotkey_error)}</span>` : ''}
    <label><input type="checkbox" data-setting="selection_anchor_enabled"${view.settings.selection_anchor_enabled ? ' checked' : ''}${view.settings.selection_anchor_supported === false ? ' disabled' : ''} />划词后显示翻译图标</label>
    <label><input type="checkbox" data-setting="selection_keyboard_fallback"${view.settings.selection_keyboard_fallback ? ' checked' : ''}${view.settings.selection_anchor_supported === false ? ' disabled' : ''} />键盘选区兜底（监听键盘活动与文本选区变化，不记录按键内容）</label>
    <label><input type="checkbox" data-setting="autostart" ${view.settings.autostart_enabled ? 'checked' : ''} />开机自启</label>
    <label><input type="checkbox" data-setting="auto_check_updates" ${view.settings.auto_check_updates ? 'checked' : ''} />启动时检查更新</label>
    <label>朗读语速 <input type="number" data-setting="tts_rate" value="${Number(view.settings.tts_rate ?? 1)}" min="0.5" max="2" step="0.1" /> ×</label>
    <label>朗读音量 <input type="number" data-setting="tts_volume" value="${Number(view.settings.tts_volume ?? 80)}" min="0" max="100" step="5" /></label>
    <label><input type="checkbox" data-setting="tts_online_fallback"${view.settings.tts_online_fallback !== false ? ' checked' : ''} />缺音色时使用在线朗读</label>
    <span class="board__hint">有本地音色时不联网；仅在该语言缺少音色时才把卡片文本发送给在线朗读服务。</span>
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
      ${HeadButton({ action: 'selection-toggle', iconName: 'selection', title: '划词翻译开关（功能待实现）', label: selectionOn ? '划词开' : '划词关' })}
      ${view.update_latest ? `<button type="button" class="icon-btn" data-action="open-release" title="下载新版 ${escapeHtml(view.update_latest)}">${escapeHtml(view.update_latest)}</button>` : ''}
      ${HeadButton({ action: 'languages', iconName: 'settings', title: '设置', label: '设置' })}
      ${HeadButton({ action: 'pin', iconName: 'pin', label: pinLabel })}
      ${HeadButton({ action: 'clear', iconName: 'clear', label: '清空' })}
    </div>
  </header>
  ${providerPanel}
  ${settingsSection}
  ${historyPanel}
  ${view.languagesOpen || view.historyOpen ? '' : `<div class="board__cards">${cards}</div>`}
</section>`;
}
