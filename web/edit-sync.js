// 编辑期同步：输入停顿后的自动提交，以及编辑中「其余语言卡如何刷新」。
//
// 为什么计时器只记语言、不捕获 textarea：卡片 DOM 可能在输入期间被语言切换、
// 面板重开等动作重建。闭包里的旧节点一旦脱离文档，提交前的 contains 检查就会
// 把这次提交静默丢掉，表现出来就是「打字后停多久都不同步」。触发时按语言找回
// 当前输入框，才拿得到用户真正敲下的最后一段文本。
//
// 为什么编辑中仍然要刷新其他卡：整板 innerHTML 替换会夺走焦点，所以过去编辑期间
// 跳过所有重绘——连带把已经回来的译文也跳过，用户只能靠失焦才看见同步结果，
// 等于把「改任一卡，其余语言自动同步」这个核心定位藏了起来。除正在编辑的卡外，
// 其余卡的数据与状态必须即时可见；正在编辑的卡文本归用户，不碰。

export const EDIT_DEBOUNCE_MS = 400;

/**
 * 按语言各自计时的输入防抖。
 * 每次输入或 compositionend 都重排该语言的计时，只有最后一次停顿会真正提交，
 * 快速切卡也不会把上一张的输入吞掉。
 */
export function createEditDebouncer({
  debounceMs = EDIT_DEBOUNCE_MS,
  setTimer = (fn, ms) => setTimeout(fn, ms),
  clearTimer = (handle) => clearTimeout(handle),
  onCommit,
} = {}) {
  const timers = new Map();

  function cancel(lang) {
    const handle = timers.get(lang);
    if (handle !== undefined) {
      clearTimer(handle);
      timers.delete(lang);
    }
  }

  return {
    schedule(lang) {
      cancel(lang);
      timers.set(
        lang,
        setTimer(() => {
          timers.delete(lang);
          onCommit(lang);
        }, debounceMs),
      );
    },
    cancel,
    cancelAll() {
      for (const handle of timers.values()) clearTimer(handle);
      timers.clear();
    },
    // 是否有任何语言还有未到期的提交：失焦收起要在这段时间内保持面板打开。
    hasPending() {
      return timers.size > 0;
    },
  };
}

/**
 * 编辑期间的重绘计划。正在编辑的卡只更新徽标、不替换元素（否则光标与输入法
 * 组字状态都会丢），其余卡按完整卡片替换。返回 null 表示没有卡片可刷新。
 */
export function planEditingRefresh(view, editingLang) {
  const cards = view.cards ?? [];
  if (!cards.length) return null;
  return {
    canClose: cards.length > 2,
    usedCodes: cards.map((card) => card.lang),
    cards: cards.map((card) => ({
      card,
      replaceElement: card.lang !== editingLang,
    })),
  };
}
