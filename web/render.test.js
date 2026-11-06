import { describe, expect, it } from 'vitest';
import { escapeHtml, renderBoard } from './render.js';

const expanded = {
  view: 'expanded',
  locked: false,
  cards: [
    { lang: 'zh', text: '你好', status: 'idle', authoritative: true },
    { lang: 'en', text: 'Hello', status: 'idle', authoritative: false },
  ],
  settings: { selection_enabled: false },
};

describe('board rendering', () => {
  it('renders one card per language with its text', () => {
    const html = renderBoard(expanded);
    expect(html.match(/<article class="card/g)).toHaveLength(2);
    expect(html).toContain('data-lang="zh"');
    expect(html).toContain('data-lang="en"');
    expect(html).toContain('你好');
    expect(html).toContain('Hello');
  });

  it('marks the authoritative card so the user knows where edits propagate from', () => {
    const html = renderBoard(expanded);
    expect(html).toContain('card--authoritative');
    expect(html).toContain('权威源');
  });

  it('shows a syncing badge without clearing the old text', () => {
    const html = renderBoard({
      ...expanded,
      cards: [expanded.cards[0], { ...expanded.cards[1], status: 'syncing' }],
    });
    expect(html).toContain('同步中');
    expect(html).toContain('Hello');
  });

  it('escapes html in card text so clipboard content cannot inject markup', () => {
    const html = renderBoard({
      ...expanded,
      cards: [{ ...expanded.cards[0], text: '<img src=x onerror=alert(1)>' }],
    });
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
    expect(escapeHtml("it's & done")).toBe('it&#39;s &amp; done');
  });

  it('returns empty string when state is not expanded (capsule is a permanent node)', () => {
    const html = renderBoard({ ...expanded, view: 'capsule' });
    expect(html).toBe('');
  });

  it('renders each card text as an editable textarea carrying the current text', () => {
    const html = renderBoard(expanded);
    expect(html.match(/<textarea class="card__text"/g)).toHaveLength(2);
    expect(html).toContain('>你好</textarea>');
  });

  it('offers a clear-all button instead of the clipboard refresh button', () => {
    const html = renderBoard(expanded);
    expect(html).toContain('data-action="clear"');
    expect(html).not.toContain('data-action="refresh"');
    expect(html).not.toContain('重读');
  });

  it('offers a retry icon only on the errored card', () => {
    const html = renderBoard({
      ...expanded,
      cards: [expanded.cards[0], { ...expanded.cards[1], status: 'error' }],
    });
    expect(html.match(/data-action="retry"/g)).toHaveLength(1);
  });

  it('hides the translation cards while settings are open and offers no language options', () => {
    expect(renderBoard(expanded)).toContain('board__cards');
    const html = renderBoard({ ...expanded, languagesOpen: true });
    expect(html).not.toContain('board__cards');
    expect(html).not.toContain('board__langs');
    expect(html).not.toContain('data-lang-code');
  });

  it('renders the provider panel with saved credentials never echoed back', () => {
    const html = renderBoard({
      ...expanded,
      languagesOpen: true,
      provider: {
        engine: 'deepl',
        has_key: true,
        has_secret: false,
        needs_secret: false,
        model: '',
        base_url: '',
      },
    });
    expect(html).toContain('data-provider="engine"');
    expect(html).toContain('value="deepl" selected');
    expect(html).toContain('type="password"');
    expect(html).toContain('已保存（留空保持）');
    expect(html).not.toContain('data-provider="model"');
    expect(html).not.toContain('data-provider="base_url"');
    expect(html).not.toContain('智谱 GLM');
    expect(html).not.toContain('value="sk-');
  });

  it('keeps the provider panel out of the default board', () => {
    const html = renderBoard(expanded);
    expect(html).not.toContain('board__provider');
    expect(html).not.toContain('data-action="save-provider"');
  });

  it('reflects the auto-read switch and hover delay in the settings section', () => {
    const html = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { auto_read_clipboard: false, hover_delay_ms: 350 },
    });
    expect(html).toContain('<input type="checkbox" data-setting="auto_read_clipboard" />');
    expect(html).toContain('value="350"');
    const on = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { auto_read_clipboard: true, hover_delay_ms: 200 },
    });
    expect(on).toContain('data-setting="auto_read_clipboard" checked');
  });

  it('gives every card a speak icon next to copy', () => {
    const html = renderBoard(expanded);
    expect(html.match(/data-action="speak"/g)).toHaveLength(2);
    expect(html.match(/data-action="copy"/g)).toHaveLength(2);
  });

  it('shows autostart switch in the settings section', () => {
    const html = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: {
        auto_read_clipboard: true,
        hover_delay_ms: 200,
        selection_enabled: false,
        selection_hotkey: 'Ctrl+Alt+B',
        autostart_enabled: true,
      },
    });
    expect(html).toContain('data-setting="autostart" checked');
    expect(html).toContain('开机自启');
  });

  // --- 新增功能测试 ---

  it('shows close buttons only when there are more than 2 cards', () => {
    // 2 张卡：不显示关闭按钮
    const two = renderBoard(expanded);
    expect(two).not.toContain('data-action="close_card"');

    // 3 张卡：每张都显示关闭按钮
    const three = renderBoard({
      ...expanded,
      cards: [
        { lang: 'zh', text: '你好', status: 'idle', authoritative: true },
        { lang: 'en', text: 'Hello', status: 'idle', authoritative: false },
        { lang: 'ja', text: 'こんにちは', status: 'idle', authoritative: false },
      ],
    });
    expect(three).toContain('data-action="close_card"');
    expect(three.match(/data-action="close_card"/g)).toHaveLength(3);
  });

  it('shows add-card button only when there are fewer than 5 cards', () => {
    // 3 张卡：显示加号
    const three = renderBoard({
      ...expanded,
      cards: [
        { lang: 'zh', text: '你好', status: 'idle', authoritative: true },
        { lang: 'en', text: 'Hello', status: 'idle', authoritative: false },
        { lang: 'ja', text: 'こんにちは', status: 'idle', authoritative: false },
      ],
    });
    expect(three).toContain('data-action="add_card"');

    // 5 张卡：加号隐藏
    const five = renderBoard({
      ...expanded,
      cards: ['zh', 'en', 'ja', 'ko', 'fr'].map((lang) => ({
        lang, text: 'x', status: 'idle', authoritative: lang === 'zh',
      })),
    });
    expect(five).not.toContain('data-action="add_card"');
  });

  it('renders autocomplete language selector on every card', () => {
    const html = renderBoard(expanded);
    expect(html).toContain('lang-select');
    expect(html.match(/data-action="change_lang"/g)).toHaveLength(2);
  });
});
