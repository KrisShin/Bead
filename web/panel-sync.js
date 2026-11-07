// 面板宽度同步：计数基线 + 增删时的「窗口 / 内容」几何计划。
//
// 窗口宽度的唯一来源是 Rust 的语言池计数：展开（resize_window）与增删后
// （sync_panel_width）各提交一次。前端只负责「什么时候该再提交一次」与
// 「内容怎么跟上」，不自行算窗口宽。
//
// 基线必须在面板显示的同一帧写入当前卡片数：展开这一次 resize 已经按语言池
// 的实时计数提交了宽度，不该被当成一次「增删」再触发同步。若基线留空，
// 展开后的第一次增删只会被记成基线，窗口不跟随 —— 收起再展开才恢复正确。

export const BOARD_INSET = 1;

export function createPanelWidthTracker() {
  let baseline = null;

  return {
    // 面板显示、窗口尺寸已由展开流程提交时调用。
    prime(count) {
      baseline = count;
    },
    // 每次渲染后调用；返回 true 表示面板可见期间的卡片数确实变了，需要同步窗口宽度。
    observe(count) {
      if (baseline === null) {
        baseline = count;
        return false;
      }
      if (count === baseline) return false;
      baseline = count;
      return true;
    },
    // 当前基线：调用方用它算出增删方向，不必再维护一份影子计数（那正是旧 bug 的来源）。
    count() {
      return baseline;
    },
    clear() {
      baseline = null;
    },
  };
}

/**
 * 卡片增删时的几何计划（全程在窗口坐标系里，单位逻辑像素）。
 *
 * 时序契约：
 * - 加卡：**先窗口、后内容**。内容先钉在旧宽（画面完全不变），窗口一次提交到新几何，
 *   再把 board 的宽度补间到新宽 —— 新卡随宽度露出。
 * - 删卡：**先内容、后窗口**。剩余卡先反向位移（FLIP）再归位，board 同时收到新宽，
 *   补间结束后才提交缩窗。
 * - 贴边（窗口左缘因 clamp 要平移 Δ≠0）：resize 的那一帧必须遮住内容，
 *   帧后立刻把 board 摆回旧屏幕矩形，再补间到新矩形。Δ=0 时全程不遮挡。
 *
 * 返回值里的矩形是 board 的内联几何（相对当前窗口）：
 * - `start`：动作开始、窗口还没变时写入，等价于当前画面。
 * - `preResizeEnd`：仅删卡有；窗口提交前内容补间的终点。
 * - `postResizeStart`：仅贴边有；窗口提交后、补间开始前 board 的落位（屏幕矩形不变）。
 * - `end`：补间终点，等于新窗口下的标准 inset 矩形。
 */
export function planPanelResize(from, to) {
  const deltaLeft = to.left - from.left;
  const edge = Math.abs(deltaLeft) > 0.5;
  const direction = to.width >= from.width ? 'expand' : 'shrink';
  // 高度不随卡片数变化，统一用旧帧高度，避免把垂直方向也卷进补间。
  const height = from.height - BOARD_INSET * 2;
  const oldWidth = from.width - BOARD_INSET * 2;
  const newWidth = to.width - BOARD_INSET * 2;
  const top = BOARD_INSET;

  if (direction === 'expand') {
    return {
      direction,
      deltaLeft,
      edge,
      windowFirst: true,
      // A：只有贴边场景需要遮住 resize 那一帧；非贴边走完整补间。
      hideAtResize: edge,
      start: { left: BOARD_INSET, top, width: oldWidth, height },
      preResizeEnd: null,
      postResizeStart: edge
        ? // 新窗口左移 Δ：把 board 内联 left 反向补 Δ，屏幕矩形与 resize 前一致
          { left: BOARD_INSET - deltaLeft, top, width: oldWidth, height }
        : null,
      end: { left: BOARD_INSET, top, width: newWidth, height },
    };
  }

  return {
    direction,
    deltaLeft,
    edge,
    windowFirst: false,
    hideAtResize: edge,
    start: { left: BOARD_INSET, top, width: oldWidth, height },
    // 窗口右缘不动：board 左缘跟着窗口要右移的量内缩，收宽发生在右缘
    preResizeEnd: {
      left: edge ? BOARD_INSET + deltaLeft : BOARD_INSET,
      top,
      width: newWidth,
      height,
    },
    // 缩窗后窗口左缘右移 Δ：把内联 left 从 1 落回 1，屏幕矩形保持不变
    postResizeStart: edge ? { left: BOARD_INSET, top, width: newWidth, height } : null,
    end: { left: BOARD_INSET, top, width: newWidth, height },
  };
}

/**
 * FLIP 的 Invert 步：剩余卡渲染后按 lang 对比首帧位置，返回需要反向位移的偏移。
 *
 * 只比较仍存在的卡（被删除的那张自动跳过）；位移小于半像素的不下发，
 * 避免把亚像素误差也变成一次过渡。
 */
export function flipOffsets(beforeRects, afterRects) {
  const previous = new Map(beforeRects.map((rect) => [rect.lang, rect]));
  return afterRects
    .map((rect) => {
      const old = previous.get(rect.lang);
      if (!old) return null;
      const dx = old.left - rect.left;
      const dy = old.top - rect.top;
      if (Math.abs(dx) < 0.5 && Math.abs(dy) < 0.5) return null;
      return { lang: rect.lang, dx, dy };
    })
    .filter(Boolean);
}
