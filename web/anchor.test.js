// 图标窗是复用的同一个 WebView：只 ShowWindow/hide，不重新加载页面。
// 这里用最小 DOM 桩把 anchor.html 的内联脚本真跑一遍，锁定「点击一次之后仍可再点」。
//
// 不引入 jsdom：项目只用 vitest，测试环境没有 DOM；桩只实现脚本实际用到的接口。

import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { afterEach, describe, expect, it, vi } from 'vitest';

const html = readFileSync(new URL('./anchor.html', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];

function createHarness(invoke) {
  const listeners = { window: {}, document: {}, button: {} };
  const dataset = {};
  const button = {
    disabled: false,
    addEventListener(name, handler) {
      listeners.button[name] = handler;
    },
  };
  const document = {
    hidden: false,
    body: { dataset },
    getElementById: () => button,
    addEventListener(name, handler) {
      listeners.document[name] = handler;
    },
  };
  const window = {
    __TAURI__: invoke ? { core: { invoke } } : undefined,
    addEventListener(name, handler) {
      listeners.window[name] = handler;
    },
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
  };

  runInNewContext(script, { window, document, console });
  return {
    button,
    dataset,
    document,
    click: () => listeners.button.click(),
    fireDocument: (name) => listeners.document[name]?.(),
    fireWindow: (name) => listeners.window[name]?.(),
  };
}

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe('selection anchor page', () => {
  it('a successful click leaves the reused button clickable for the next selection', async () => {
    const invoke = vi.fn().mockResolvedValue({ view: 'expanded' });
    const harness = createHarness(invoke);

    await harness.click();
    expect(invoke).toHaveBeenCalledTimes(1);
    // 关键回归点：旧实现成功后 button.disabled 保持 true，下一次图标出现点不动。
    expect(harness.button.disabled).toBe(false);

    await harness.click();
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it('a stale pending selection shows a missed hint but still re-enables the button', async () => {
    vi.useFakeTimers();
    const harness = createHarness(vi.fn().mockResolvedValue(null));

    await harness.click();
    expect(harness.dataset.missed).toBe('1');
    expect(harness.button.disabled).toBe(false);

    vi.advanceTimersByTime(500);
    expect(harness.dataset.missed).toBeUndefined();
  });

  it('a command error never leaves the button disabled', async () => {
    vi.useFakeTimers();
    const harness = createHarness(vi.fn().mockRejectedValue(new Error('bridge down')));

    await harness.click();
    expect(harness.dataset.missed).toBe('1');
    expect(harness.button.disabled).toBe(false);
  });

  it('showing the window again clears leftover disabled and missed state', async () => {
    vi.useFakeTimers();
    const harness = createHarness(vi.fn().mockResolvedValue(null));

    await harness.click();
    harness.button.disabled = true; // 模拟上一轮异常留下的脏状态
    harness.document.hidden = false;
    harness.fireDocument('visibilitychange');

    expect(harness.button.disabled).toBe(false);
    expect(harness.dataset.missed).toBeUndefined();
  });

  it('a second click while the first is still in flight is ignored', async () => {
    let resolve;
    const invoke = vi.fn(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const harness = createHarness(invoke);

    const first = harness.click();
    const second = harness.click();
    expect(invoke).toHaveBeenCalledTimes(1);
    resolve({ view: 'expanded' });
    await Promise.all([first, second]);
    expect(harness.button.disabled).toBe(false);
  });
});
