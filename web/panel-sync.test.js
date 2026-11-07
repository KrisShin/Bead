import { describe, expect, it } from 'vitest';
import {
  createPanelWidthTracker,
  flipOffsets,
  planPanelResize,
} from './panel-sync.js';

const frame = (left, width) => ({ left, top: 0, width, height: 300 });

describe('panel width sync baseline', () => {
  it('syncs the window when the first card added after the panel opens changes the count', () => {
    const tracker = createPanelWidthTracker();
    tracker.prime(3); // 展开时窗口已按 3 张卡提交宽度

    expect(tracker.observe(4)).toBe(true);
  });

  it('syncs the window when the first card removed after the panel opens changes the count', () => {
    const tracker = createPanelWidthTracker();
    tracker.prime(4);

    expect(tracker.observe(3)).toBe(true);
  });

  it('does not sync again for the resize the expand itself already committed', () => {
    const tracker = createPanelWidthTracker();
    tracker.prime(4);

    expect(tracker.observe(4)).toBe(false);
  });

  it('keeps the window still while repeated renders keep the same card count', () => {
    const tracker = createPanelWidthTracker();
    tracker.prime(3);

    expect(tracker.observe(5)).toBe(true);
    expect(tracker.observe(5)).toBe(false);
    expect(tracker.observe(5)).toBe(false);
  });

  it('rebases on the next expand so its own card count is not treated as a change', () => {
    const tracker = createPanelWidthTracker();
    tracker.prime(3);
    tracker.observe(4);
    tracker.clear(); // 收起
    tracker.prime(4); // 再展开，窗口已按 4 张卡提交

    expect(tracker.observe(4)).toBe(false);
    expect(tracker.observe(3)).toBe(true);
  });

  it('reports the current baseline so callers do not keep a shadow count', () => {
    const tracker = createPanelWidthTracker();
    expect(tracker.count()).toBe(null);

    tracker.prime(3);
    tracker.observe(4);

    expect(tracker.count()).toBe(4);
  });
});

describe('panel resize plan', () => {
  it('an expand pins the content to the old width until the window has grown', () => {
    const plan = planPanelResize(frame(728, 613), frame(728, 812));

    expect(plan.direction).toBe('expand');
    expect(plan.windowFirst).toBe(true);
    expect(plan.start).toEqual({ left: 1, top: 1, width: 611, height: 298 });
    expect(plan.end).toEqual({ left: 1, top: 1, width: 810, height: 298 });
    expect(plan.hideAtResize).toBe(false);
    expect(plan.postResizeStart).toBe(null);
  });

  it('an expand at the screen edge hides one frame and restores the old screen rect', () => {
    // 贴右缘：加一张卡后窗口左缘左移 199，右缘不动
    const from = frame(1307, 613);
    const to = frame(1108, 812);
    const plan = planPanelResize(from, to);

    expect(plan.edge).toBe(true);
    expect(plan.hideAtResize).toBe(true);
    // 旧屏幕左缘 = 1307 + 1；新窗口左缘 = 1108 → 内联 left = 200
    expect(plan.postResizeStart).toEqual({ left: 200, top: 1, width: 611, height: 298 });
    expect(plan.end).toEqual({ left: 1, top: 1, width: 810, height: 298 });
  });

  it('a shrink tightens the content before the window shrinks', () => {
    const plan = planPanelResize(frame(728, 812), frame(728, 613));

    expect(plan.direction).toBe('shrink');
    expect(plan.windowFirst).toBe(false);
    expect(plan.preResizeEnd).toEqual({ left: 1, top: 1, width: 611, height: 298 });
    expect(plan.end).toEqual({ left: 1, top: 1, width: 611, height: 298 });
    expect(plan.hideAtResize).toBe(false);
  });

  it('a shrink at the screen edge keeps the right edge until the window moves', () => {
    // 贴右缘：删一张卡后窗口左缘右移 199，右缘不动
    const from = frame(1108, 812);
    const to = frame(1307, 613);
    const plan = planPanelResize(from, to);

    expect(plan.edge).toBe(true);
    expect(plan.hideAtResize).toBe(true);
    // 窗口提交前 board 左缘内缩 199，右缘留在原窗口右缘
    expect(plan.preResizeEnd).toEqual({ left: 200, top: 1, width: 611, height: 298 });
    // 缩窗后内联 left 落回 1，屏幕矩形不变
    expect(plan.postResizeStart).toEqual({ left: 1, top: 1, width: 611, height: 298 });
  });
});

describe('removal flip', () => {
  it('removing the last card moves no remaining card', () => {
    const before = [
      { lang: 'zh', left: 0, top: 0 },
      { lang: 'en', left: 199, top: 0 },
      { lang: 'ja', left: 398, top: 0 },
      { lang: 'fr', left: 597, top: 0 },
    ];
    const after = before.slice(0, 3);

    expect(flipOffsets(before, after)).toEqual([]);
  });

  it('removing a middle card pushes the following cards back to their old places', () => {
    const before = [
      { lang: 'zh', left: 0, top: 0 },
      { lang: 'en', left: 199, top: 0 },
      { lang: 'ja', left: 398, top: 0 },
      { lang: 'fr', left: 597, top: 0 },
    ];
    const after = [
      { lang: 'zh', left: 0, top: 0 },
      { lang: 'ja', left: 199, top: 0 },
      { lang: 'fr', left: 398, top: 0 },
    ];

    expect(flipOffsets(before, after)).toEqual([
      { lang: 'ja', dx: 199, dy: 0 },
      { lang: 'fr', dx: 199, dy: 0 },
    ]);
  });
});
