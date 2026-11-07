import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EDIT_DEBOUNCE_MS, createEditDebouncer, planEditingRefresh } from './edit-sync.js';

const syncedView = {
  cards: [
    { lang: 'zh', text: '你好', status: 'idle', authoritative: true },
    { lang: 'en', text: 'Hello', status: 'idle', authoritative: false },
    { lang: 'ja', text: 'こんにちは', status: 'idle', authoritative: false },
  ],
};

describe('editing a language card', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('typing a card and pausing for 400ms updates every other language card', () => {
    const editor = { value: '' };
    const commits = [];
    const debouncer = createEditDebouncer({
      onCommit: (lang) => commits.push({ lang, text: editor.value }),
    });

    editor.value = '你好';
    debouncer.schedule('zh');
    vi.advanceTimersByTime(EDIT_DEBOUNCE_MS);

    expect(commits).toEqual([{ lang: 'zh', text: '你好' }]);
    // Rust 翻译完成推回快照：编辑中的卡不被替换，其余语言卡必须带上新译文。
    const refreshed = planEditingRefresh(syncedView, 'zh').cards.filter(
      (entry) => entry.replaceElement,
    );
    expect(refreshed.map((entry) => entry.card.lang)).toEqual(['en', 'ja']);
    expect(refreshed.map((entry) => entry.card.text)).toEqual(['Hello', 'こんにちは']);
  });

  it('keeps the card being edited untouched so the text and caret survive', () => {
    const plan = planEditingRefresh(syncedView, 'zh');
    expect(plan.cards.find((entry) => entry.card.lang === 'zh').replaceElement).toBe(false);
  });

  it('restarts the pause on each keystroke so only the last pause commits', () => {
    const commits = [];
    const debouncer = createEditDebouncer({ onCommit: (lang) => commits.push(lang) });

    debouncer.schedule('zh');
    vi.advanceTimersByTime(EDIT_DEBOUNCE_MS - 50);
    debouncer.schedule('zh');
    vi.advanceTimersByTime(EDIT_DEBOUNCE_MS - 50);
    expect(commits).toEqual([]);

    vi.advanceTimersByTime(50);
    expect(commits).toEqual(['zh']);
  });

  it('does not keep a pending pause after blur or enter commits immediately', () => {
    const commits = [];
    const debouncer = createEditDebouncer({ onCommit: (lang) => commits.push(lang) });

    debouncer.schedule('zh');
    debouncer.cancel('zh');
    vi.advanceTimersByTime(EDIT_DEBOUNCE_MS);

    expect(commits).toEqual([]);
  });

  it('re-arms the pause after a composition ends so the chosen text still syncs', () => {
    const commits = [];
    const debouncer = createEditDebouncer({ onCommit: (lang) => commits.push(lang) });

    debouncer.schedule('zh');
    debouncer.cancel('zh'); // 组字期间到期被 composing 挡下
    debouncer.schedule('zh'); // compositionend 补排

    vi.advanceTimersByTime(EDIT_DEBOUNCE_MS);
    expect(commits).toEqual(['zh']);
  });
});
