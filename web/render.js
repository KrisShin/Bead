// 纯展示层：输入 Rust 给的 state 快照，输出 HTML 字符串。
// 组件全部在 components/ 目录中定义，这里只做组装。

import { escapeHtml } from './components/escape.js';
import { icon } from './components/icons.js';
import {
  Capsule,
  HeadButton,
  Card,
  AddCardButton,
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
    <label><input type="checkbox" data-setting="selection_enabled"${view.settings.selection_enabled ? ' checked' : ''} />划词（${view.settings.selection_hotkey}，改键重启生效）</label>
    <label><input type="checkbox" data-setting="autostart" ${view.settings.autostart_enabled ? 'checked' : ''} />开机自启</label>
    <label><input type="checkbox" data-setting="auto_check_updates" ${view.settings.auto_check_updates ? 'checked' : ''} />启动时检查更新</label>
    <button type="button" data-action="check-updates">检查更新</button>
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
  ${view.languagesOpen ? '' : `<div class="board__cards">${cards}</div>`}
</section>`;
}
