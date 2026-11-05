// 纯展示层：输入 Rust 给的 state 快照，输出 HTML 字符串。
// 这里不做任何业务判断（防抖、去重、竞态全在 bead-core），也就不该有第二种渲染来源：
// 视觉快照直接让浏览器加载本文件所在的页面，和 Tauri 里看到的是同一套组件与样式。

const STATUS_LABEL = {
  idle: '',
  syncing: '同步中',
  error: '同步失败',
};

// 设置面板的候选语言：覆盖常见语系；拉丁系彼此不可字符判别，落卡靠检测回落主语言。
const LANG_CANDIDATES = ['zh', 'en', 'ja', 'ko', 'fr', 'de', 'es', 'ru'];

// 引擎清单：code 与 Rust Engine::code() 一一对应。
const ENGINES = [
  ['zerokey', '默认（免费）'],
  ['zhipu', '智谱 GLM'],
  ['siliconflow', '硅基流动'],
  ['custom', '自定义 OpenAI 兼容'],
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

export function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

export function renderBoard(view) {
  if (view.view !== 'expanded') {
    return '<div id="capsule" role="button" aria-label="展开 Bead"><span>译</span></div>';
  }

  const cards = view.cards
    .map((card) => {
      const classes = ['card'];
      if (card.authoritative) classes.push('card--authoritative');
      if (card.status === 'error') classes.push('card--error');
      const lang = escapeHtml(card.lang);
      const status = STATUS_LABEL[card.status] ?? '';
      return `<article class="${classes.join(' ')}" data-lang="${lang}">
  <header class="card__head">
    <span class="card__lang">${lang}</span>
    ${card.authoritative ? '<span class="card__badge">权威源</span>' : ''}
    ${status ? `<span class="card__status">${status}</span>` : ''}
  </header>
  <textarea class="card__text" data-lang="${lang}" spellcheck="false">${escapeHtml(card.text)}</textarea>
  <footer class="card__actions">
    ${card.status === 'error' ? `<button type="button" data-action="retry" data-lang="${lang}">重试</button>` : ''}
    <button type="button" data-action="speak" data-lang="${lang}">朗读</button>
    <button type="button" data-action="copy" data-lang="${lang}">复制</button>
  </footer>
</article>`;
    })
    .join('');

  const languagePanel = view.languagesOpen
    ? `<div class="board__langs">${LANG_CANDIDATES.map((code) => {
        const checked = view.cards.some((card) => card.lang === code);
        const count = view.cards.length;
        const disabled = (!checked && count >= 5) || (checked && count <= 2);
        return `<label class="board__lang${disabled ? ' is-disabled' : ''}"><input type="checkbox" data-lang-code="${code}"${checked ? ' checked' : ''}${disabled ? ' disabled' : ''} />${code}</label>`;
      }).join('')}</div>`
    : '';

  // key/secret 一律不回显：只渲染密码框和「已保存」占位（规范第 04 条工单要求）。
  const provider = view.provider ?? EMPTY_PROVIDER;
  const providerPanel = view.languagesOpen
    ? `<div class="board__provider">
    <select data-provider="engine">${ENGINES.map(
        ([code, label]) =>
          `<option value="${code}"${provider.engine === code ? ' selected' : ''}>${label}</option>`,
      ).join('')}</select>
    <input type="password" data-provider="api_key" placeholder="${provider.has_key ? '已保存（留空保持）' : '密钥'}" />
    <input type="password" data-provider="secret" placeholder="${provider.has_secret ? '已保存（留空保持）' : '第二密钥（腾讯/火山/阿里/百度/有道）'}" />
    <input type="text" data-provider="model" value="${escapeHtml(provider.model)}" placeholder="模型（留空用默认）" list="model-list" />
    <datalist id="model-list"></datalist>
    <input type="text" data-provider="base_url" value="${escapeHtml(provider.base_url)}" placeholder="base_url（仅自定义引擎，含 /v1）" />
    <button type="button" data-action="save-provider">保存</button>
    <button type="button" data-action="test-provider">测试连接</button>
    <button type="button" data-action="list-models">拉模型</button>
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

  return `<section id="board" class="board">
  <header class="board__head">
    <span class="board__title">Bead · 悬停即译</span>
    ${view.update_latest ? `<button type="button" data-action="open-release" title="前往下载">新版 ${escapeHtml(view.update_latest)}</button>` : ''}
    <button type="button" data-action="languages">设置</button>
    <button type="button" data-action="pin">${view.locked ? '已锁定' : '图钉'}</button>
    <button type="button" data-action="refresh" title="忽略去重，重新读取剪贴板">重读</button>
  </header>
  ${languagePanel}
  ${providerPanel}
  ${settingsSection}
  ${view.notice ? `<p class="board__notice">${escapeHtml(view.notice)}</p>` : ''}
  <div class="board__cards">${cards}</div>
  <p class="board__hint">点击卡片直接编辑 · 其余语言自动同步 · ESC 收起</p>
</section>`;
}
