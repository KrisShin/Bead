// 自绘 SVG 图标库：统一 16x16 viewbox，currentColor 继承父元素颜色。
// 所有图标都是纯 SVG 字符串，render.js 直接嵌入 HTML。

export const ICONS = {
  // 设置（滑杆）：三档横向滑轨 + 实心滑块，16px 下比齿轮更干净、辨识度更高。
  settings: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    <circle cx="6" cy="4.5" r="1.5" fill="currentColor"/>
    <circle cx="10.5" cy="8" r="1.5" fill="currentColor"/>
    <circle cx="5" cy="11.5" r="1.5" fill="currentColor"/>
  </svg>`,

  // OCR 系统方案（显示器 + 文本行）：强调「系统自带接口」。
  ocr_system: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect x="2" y="3" width="12" height="7.6" rx="1.2" stroke="currentColor" stroke-width="1.2"/>
    <path d="M4.8 5.7h6.4M4.8 8h4" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>
    <path d="M8 10.6v2.8M6 13.4h4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>`,

  // OCR 内置方案（芯片）：强调「自带插件 / 本地模型」。
  ocr_builtin: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect x="4.6" y="4.6" width="6.8" height="6.8" rx="1.2" stroke="currentColor" stroke-width="1.2"/>
    <rect x="6.9" y="6.9" width="2.2" height="2.2" rx="0.5" fill="currentColor"/>
    <path d="M6.2 2.6v1.6M9.8 2.6v1.6M6.2 11.8v1.6M9.8 11.8v1.6M2.6 6.2h1.6M2.6 9.8h1.6M11.8 6.2h1.6M11.8 9.8h1.6"
      stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>
  </svg>`,

  // 图钉/锁定（pushpin；旧的十字星形容易被看成太阳）
  pin: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M5.6 2h4.8l-.5 3.6 1.6 1.6v.9H4.5v-.9l1.6-1.6L5.6 2Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
    <path d="M8 8.1V14" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>`,

  // 翻译历史（时钟）：与其它图标同一套 16×16 描边、currentColor。
  history: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <circle cx="8" cy="8" r="5.6" stroke="currentColor" stroke-width="1.2"/>
    <path d="M8 4.4V8l2.5 1.7" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`,

  // 闭合锁：已锁定。锁梁两条腿都落到锁体上，和下面的开口锁一眼可分。
  lock: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect x="3.5" y="7.3" width="9" height="5.9" rx="1.2" stroke="currentColor" stroke-width="1.2"/>
    <path d="M5.6 7.3V5.3a2.4 2.4 0 0 1 4.8 0v2" stroke="currentColor" stroke-width="1.2"/>
    <path d="M8 9.6v1.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>`,

  // 开口锁：未锁定。锁梁右腿悬空，缺少「v2」这段收尾。
  unlock: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect x="3.5" y="7.3" width="9" height="5.9" rx="1.2" stroke="currentColor" stroke-width="1.2"/>
    <path d="M5.6 7.3V5.3a2.4 2.4 0 0 1 4.6-1.1" stroke="currentColor" stroke-width="1.2"/>
    <path d="M8 9.6v1.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>`,

  // 清空（垃圾桶）
  clear: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M2.5 4h11M6 4V2.5h4V4M4 4l.6 9.2A1 1 0 0 0 5.6 14h4.8a1 1 0 0 0 1-.8L12 4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M6.5 6.5v4M9.5 6.5v4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>`,

  // 朗读/喇叭
  speaker: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M2 5.5h2.5L8 2.5v11l-3.5-3H2v-5Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
    <path d="M10.5 5.5c.7.7 1.1 1.6 1.1 2.5s-.4 1.8-1.1 2.5M12.5 3.5c1.3 1.3 2 3.1 2 4.5s-.7 3.2-2 4.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
  </svg>`,

  // 复制
  copy: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect x="5" y="5" width="8" height="8" rx="1.2" stroke="currentColor" stroke-width="1.2"/>
    <path d="M11 5V3.8A1.2 1.2 0 0 0 9.8 2.6H3.8A1.2 1.2 0 0 0 2.6 3.8v6a1.2 1.2 0 0 0 1.2 1.2H5"
    stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`,

  // 关闭/X（对角线撑满 viewBox，14px 下也能看清是叉而不是点）
  close: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M2.5 2.5l11 11M13.5 2.5l-11 11" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/>
  </svg>`,

  // 加号
  plus: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M8 3v10M3 8h10" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
  </svg>`,

  // 重试
  retry: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M3 8a5 5 0 0 1 8.5-3.5M13 8a5 5 0 0 1-8.5 3.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
    <path d="M11.5 3.5v2.5H9M4.5 12.5V10h2.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`,

  // 搜索/下拉指示
  chevron: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M4.5 6l3.5 3.5L11.5 6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`,

  // 划词翻译开启：完整的 AB 字形。
  selection: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M2.5 12V4M2.5 8h3.5a2 2 0 1 1 0 4H2.5M2.5 8h3.5a2 2 0 1 0 0-4H2.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    <path d="M9.5 12V5.5M9.5 5.5h3M9.5 9.5h2.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
  </svg>`,

  // 划词翻译关闭：同字形被一条对角斜线划断，形状本身即可区分两态。
  selection_off: `<svg viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
    <path d="M2.5 12V4M2.5 8h3.5a2 2 0 1 1 0 4H2.5M2.5 8h3.5a2 2 0 1 0 0-4H2.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    <path d="M9.5 12V5.5M9.5 5.5h3M9.5 9.5h2.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M12.9 3.1 3.1 12.9" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
  </svg>`,
};

/**
 * 获取图标 SVG 字符串
 * @param {string} name 图标名
 * @param {Object} opts { size, className, title }
 */
export function icon(name, opts = {}) {
  const svg = ICONS[name] || ICONS.close;
  const size = opts.size || 16;
  const cls = opts.className ? ` class="${opts.className}"` : '';
  const title = opts.title ? `<title>${opts.title}</title>` : '';
  return svg
    .replace('<svg ', `<svg width="${size}" height="${size}"${cls} role="img" aria-hidden="true" `)
    .replace('>', `>${title}`);
}
