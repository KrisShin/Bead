// 纯展示层：输入 Rust 给的 state 快照，输出 HTML 字符串。
// 这里不做任何业务判断（防抖、去重、竞态全在 bead-core），也就不该有第二种渲染来源：
// 视觉快照直接让浏览器加载本文件所在的页面，和 Tauri 里看到的是同一套组件与样式。

const STATUS_LABEL = {
  idle: '',
  syncing: '同步中',
  error: '同步失败',
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
  <div class="card__text">${escapeHtml(card.text)}</div>
  <footer class="card__actions">
    <button type="button" data-action="copy" data-lang="${lang}">复制</button>
  </footer>
</article>`;
    })
    .join('');

  return `<section id="board" class="board">
  <header class="board__head">
    <span class="board__title">Bead · 悬停即译</span>
    <button type="button" data-action="pin">${view.locked ? '已锁定' : '图钉'}</button>
    <button type="button" data-action="refresh" title="忽略去重，重新读取剪贴板">重读</button>
  </header>
  ${view.notice ? `<p class="board__notice">${escapeHtml(view.notice)}</p>` : ''}
  <div class="board__cards">${cards}</div>
  <p class="board__hint">ESC 收起 · 编辑联动见工单 02</p>
</section>`;
}
