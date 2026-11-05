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

  return `<section id="board" class="board">
  <header class="board__head">
    <span class="board__title">Bead · 悬停即译</span>
    <button type="button" data-action="languages">语言</button>
    <button type="button" data-action="pin">${view.locked ? '已锁定' : '图钉'}</button>
    <button type="button" data-action="refresh" title="忽略去重，重新读取剪贴板">重读</button>
  </header>
  ${languagePanel}
  ${view.notice ? `<p class="board__notice">${escapeHtml(view.notice)}</p>` : ''}
  <div class="board__cards">${cards}</div>
  <p class="board__hint">点击卡片直接编辑 · 其余语言自动同步 · ESC 收起</p>
</section>`;
}
