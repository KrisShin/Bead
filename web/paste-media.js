/**
 * 卡片内粘贴图片的缩略图归属规则（纯函数，供 app.js 与单测共用）。
 *
 * 缩略图不挂在 Document slot 上，只在前端内存里：它属于「粘贴当时那张卡的那段文本」。
 * 文档内容一旦被新的截图 OCR / 新文本 / 清空 / 历史回填替换，这段文本就不存在了，
 * 缩略图也必须一起消失，否则会出现「文字已是新内容、图片还是旧截图」的错位。
 * - 卡片已不存在（清空文档 / 语言池移除该卡）→ 过期；
 * - 识别已结束但卡片文本已变 → 过期（识别失败时的基线文本同样按这条判定）；
 * - 识别中（pending）→ 保留：这期间卡片文本还可能被翻译刷新，不能按文本比对误删。
 * 返回需要丢弃的语言列表，由调用方回收 objectURL。
 */
export function stalePasteLangs(entries, cards) {
  const texts = new Map((cards ?? []).map((card) => [card.lang, card.text]));
  const stale = [];
  for (const [lang, entry] of entries) {
    if (!texts.has(lang)) {
      stale.push(lang);
      continue;
    }
    if (entry?.pending) continue;
    // `text` 缺失视为已过期：没有可归属的文本就没有理由继续显示缩略图。
    if (entry?.text !== texts.get(lang)) stale.push(lang);
  }
  return stale;
}
