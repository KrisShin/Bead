// 卡片拖拽排序：只产出「能不能拖」「目标顺序」「要不要补间」这类可判定的纯逻辑。
// DOM 位移、FLIP 与视觉状态由 app.js 负责；窗口拖动只绑在胶囊与面板头上，
// 卡片拖拽不会走进 start_drag，因此这里不涉及任何窗口几何。
//
// 为什么重排要拦编辑态：重排会移动 DOM 节点，正在组字的 textarea 会丢焦点/丢
// composition 状态；此时宁可这一次拖拽不生效，也不去打断 edit-sync 的提交链路。

export const CARD_REORDER_MS = 160;

/**
 * 起拖前置条件：面板已展开且稳定、不在编辑态、按在卡片顶部的拖拽把手上。
 * `busy` 表示面板正在进行展开 / 增删补间（此时卡片自带 keyframes，FLIP 会被覆盖）。
 */
export function canStartCardDrag({ expanded, editing = false, onGrip = false, busy = false }) {
  return Boolean(expanded) && !editing && Boolean(onGrip) && !busy;
}

/** 落点在目标的左半区插到它前面，右半区插到它后面。 */
export function insertionSide(clientX, rect) {
  return clientX < rect.left + rect.width / 2 ? 'before' : 'after';
}

/**
 * 目标顺序：先摘掉被拖的卡，再按目标卡 + 半边插回去。
 * 返回新数组；拖回原位时结果与入参相同，由 orderChanged 判定不必写回 Rust。
 */
export function planCardReorder(order, draggedLang, overLang, side) {
  if (!order.includes(draggedLang) || overLang === draggedLang) return order.slice();
  const rest = order.filter((lang) => lang !== draggedLang);
  const index = rest.indexOf(overLang);
  if (index < 0) return order.slice();
  const at = side === 'after' ? index + 1 : index;
  return [...rest.slice(0, at), draggedLang, ...rest.slice(at)];
}

/** 顺序没变就不发 reorder_cards：拖回原位不该产生一次语言池写入。 */
export function orderChanged(previous, next) {
  return previous.length !== next.length || previous.some((lang, i) => lang !== next[i]);
}

/** reduced-motion 下直接落位（0 = 不建 transition，也不做反向位移）。 */
export function cardReorderDuration(reducedMotion) {
  return reducedMotion ? 0 : CARD_REORDER_MS;
}
