// 公共组件：每个组件是纯函数，输入数据返回 HTML 字符串。
// render.js 里直接引用这些组件，避免 HTML 片段散落。

import { escapeHtml } from './escape.js';
import { icon } from './icons.js';

const STATUS_LABEL = {
  idle: '',
  syncing: '同步中',
  error: '同步失败',
};

const LANG_NAMES = {
  zh: '中文', en: 'English', ja: '日本語', ko: '한국어',
  fr: 'Français', de: 'Deutsch', es: 'Español', ru: 'Русский',
  pt: 'Português', it: 'Italiano', ar: 'العربية', th: 'ไทย',
  vi: 'Tiếng Việt', id: 'Bahasa', nl: 'Nederlands', tr: 'Türkçe',
};

/** 胶囊组件 */
export function Capsule() {
  return `<div id="capsule" role="button" aria-label="展开 Bead" title="悬停展开 · 按住可拖动位置"><span>译</span></div>`;
}

/** 面板头按钮（带图标） */
export function HeadButton({ action, iconName, label, title }) {
  return `<button type="button" class="icon-btn" data-action="${action}"${title ? ` title="${title}"` : ''} data-tooltip="${escapeHtml(label)}">
    ${icon(iconName, { size: 16, className: 'icon-btn__svg' })}
  </button>`;
}

/** 卡片上的图标按钮 */
export function CardButton({ action, lang, iconName, title }) {
  return `<button type="button" class="icon-btn icon-btn--sm" data-action="${action}" data-lang="${lang}" title="${title}">
    ${icon(iconName, { size: 14, className: 'icon-btn__svg' })}
  </button>`;
}

/** 关闭按钮（卡片 hover 显示） */
export function CloseButton({ lang }) {
  return `<button type="button" class="card__close" data-action="close_card" data-lang="${lang}" title="关闭此卡片">
    ${icon('close', { size: 12, className: 'card__close-svg' })}
  </button>`;
}

/** 加号按钮（添加卡片） */
export function AddCardButton() {
  return `<button type="button" class="icon-btn icon-btn--add" data-action="add_card" title="添加一张卡片（最多 5 张）">
    ${icon('plus', { size: 16, className: 'icon-btn__svg' })}
  </button>`;
}

/** 语言自动完成下拉（单张卡片上方） */
export function LangAutocomplete({ current, usedCodes }) {
  // 所有候选语言，过滤掉已被其他卡片使用的
  const ALL = Object.keys(LANG_NAMES);
  const available = ALL.filter((code) => !usedCodes.includes(code) || code === current);
  const disabledSet = new Set(usedCodes.filter((c) => c !== current));

  const options = available
    .map((code) => {
      const selected = code === current ? ' selected' : '';
      const dis = disabledSet.has(code) ? ' disabled' : '';
      return `<option value="${code}"${selected}${dis}>${LANG_NAMES[code] || code} (${code})</option>`;
    })
    .join('');

  return `<div class="lang-select" data-lang="${current}">
    <select class="lang-select__select" data-action="change_lang" data-lang="${current}">
      ${options}
    </select>
    ${icon('chevron', { size: 12, className: 'lang-select__chevron' })}
  </div>`;
}

/** 单张翻译卡片 */
export function Card({ card, canClose, usedCodes }) {
  const classes = ['card'];
  if (card.authoritative) classes.push('card--authoritative');
  if (card.status === 'error') classes.push('card--error');
  const lang = escapeHtml(card.lang);
  const status = STATUS_LABEL[card.status] ?? '';

  // 其他卡片已用的语言（不包括自己）
  const otherUsed = usedCodes.filter((c) => c !== card.lang);

  return `<article class="${classes.join(' ')}" data-lang="${lang}" draggable="true">
  <header class="card__head">
    ${canClose ? CloseButton({ lang: card.lang }) : ''}
    ${LangAutocomplete({ current: card.lang, usedCodes: otherUsed })}
    ${card.authoritative ? '<span class="card__badge">权威源</span>' : ''}
    ${status ? `<span class="card__status">${status}</span>` : ''}
  </header>
  <textarea class="card__text" data-lang="${lang}" spellcheck="false">${escapeHtml(card.text)}</textarea>
  <footer class="card__actions">
    ${card.status === 'error' ? CardButton({ action: 'retry', lang: card.lang, iconName: 'retry', title: '重试' }) : ''}
    ${CardButton({ action: 'speak', lang: card.lang, iconName: 'speaker', title: '朗读' })}
    ${CardButton({ action: 'copy', lang: card.lang, iconName: 'copy', title: '复制' })}
  </footer>
</article>`;
}
