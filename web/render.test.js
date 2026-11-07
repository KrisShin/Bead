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

  it('shows a closed lock and reports pressed state when the board is locked', () => {
    const html = renderBoard({ ...expanded, locked: true });
    const pin = html.match(/<button[^>]*data-action="pin"[\s\S]*?<\/button>/)?.[0];

    expect(pin).toContain('class="icon-btn icon-btn--lock"');
    expect(pin).toContain('aria-pressed="true"');
    expect(pin).toContain('aria-label="锁定"');
    expect(pin).toContain('data-tooltip="已锁定（点击解锁）"');
    // 闭合锁：锁梁右腿落到锁体（v2），不是开口锁那段悬空的弧。
    expect(pin).toContain('4.8 0v2');
    expect(pin).not.toContain('4.6-1.1');
  });

  it('shows an open lock and reports unpressed state when the board is unlocked', () => {
    const pin = renderBoard(expanded)
      .match(/<button[^>]*data-action="pin"[\s\S]*?<\/button>/)?.[0];

    expect(pin).toContain('aria-pressed="false"');
    expect(pin).toContain('data-tooltip="锁定"');
    // 开口锁：锁梁右腿悬空。
    expect(pin).toContain('4.6-1.1');
    expect(pin).not.toContain('4.8 0v2');
  });

  it('shows a slashed AB icon and reports unpressed when selection is off', () => {
    const toggle = renderBoard(expanded)
      .match(/<button[^>]*data-action="selection-toggle"[\s\S]*?<\/button>/)?.[0];

    // 原生 button + type=button：键盘可聚焦、可回车/空格触发。
    expect(toggle).toMatch(/^<button type="button"/);
    expect(toggle).toContain('class="icon-btn icon-btn--selection"');
    expect(toggle).toContain('aria-pressed="false"');
    expect(toggle).toContain('aria-label="划词翻译"');
    expect(toggle).toContain('data-tooltip="划词翻译已关闭（点击开启）"');
    // 关闭态：AB 字形被对角斜线划断，和开启态的完整 AB 形状不同。
    expect(toggle).toContain('M12.9 3.1 3.1 12.9');
  });

  it('shows the full AB icon and reports pressed when selection is on', () => {
    const toggle = renderBoard({ ...expanded, settings: { selection_enabled: true } })
      .match(/<button[^>]*data-action="selection-toggle"[\s\S]*?<\/button>/)?.[0];

    expect(toggle).toContain('class="icon-btn icon-btn--selection"');
    expect(toggle).toContain('aria-pressed="true"');
    expect(toggle).toContain('aria-label="划词翻译"');
    expect(toggle).toContain('data-tooltip="划词翻译已开启（点击关闭）"');
    expect(toggle).not.toContain('M12.9 3.1 3.1 12.9');
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

  it('shows the selection anchor switch and greys it out when unsupported', () => {
    const on = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { selection_anchor_enabled: true, selection_anchor_supported: true },
    });
    expect(on).toContain('data-setting="selection_anchor_enabled" checked');
    expect(on).not.toContain('data-setting="selection_anchor_enabled" checked disabled');

    const unsupported = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { selection_anchor_enabled: false, selection_anchor_supported: false },
    });
    expect(unsupported).toContain('data-setting="selection_anchor_enabled" disabled');
  });

  it('renders a hotkey input and surfaces registration failures instead of hiding them', () => {
    const html = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { selection_hotkey: 'Ctrl+Alt+Shift+B' },
    });
    expect(html).toContain('data-setting="selection_hotkey"');
    expect(html).toContain('value="Ctrl+Alt+Shift+B"');

    const failed = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: {
        selection_hotkey: 'Ctrl+Alt+Shift+B',
        selection_hotkey_error: '快捷键被其他程序占用（错误 1409），请改键或改用划词图标',
      },
    });
    expect(failed).toContain('board__hint');
    expect(failed).toContain('快捷键被其他程序占用');
    expect(failed).toContain('请改键或改用划词图标');
  });

  it('shows the keyboard fallback switch as off by default and disabled when unsupported', () => {
    const off = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { selection_keyboard_fallback: false, selection_anchor_supported: true },
    });
    expect(off).toContain('data-setting="selection_keyboard_fallback"');
    expect(off).not.toContain('data-setting="selection_keyboard_fallback" checked');
    expect(off).toContain('不记录按键内容');

    const on = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { selection_keyboard_fallback: true, selection_anchor_supported: true },
    });
    expect(on).toContain('data-setting="selection_keyboard_fallback" checked');

    const unsupported = renderBoard({
      ...expanded,
      languagesOpen: true,
      settings: { selection_keyboard_fallback: false, selection_anchor_supported: false },
    });
    expect(unsupported).toContain('data-setting="selection_keyboard_fallback" disabled');
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

  it('renders the close icon as a 14px cross, not a dot', () => {
    const three = renderBoard({
      ...expanded,
      cards: [
        { lang: 'zh', text: '你好', status: 'idle', authoritative: true },
        { lang: 'en', text: 'Hello', status: 'idle', authoritative: false },
        { lang: 'ja', text: 'こんにちは', status: 'idle', authoritative: false },
      ],
    });
    const closeButton = three.match(/<button[^>]*class="card__close"[\s\S]*?<\/button>/)?.[0];

    expect(closeButton).toContain('width="14" height="14"');
    expect(closeButton).toContain('M2.5 2.5l11 11M13.5 2.5l-11 11');
  });

  it('gives every card a drag grip and keeps the card body undraggable', () => {
    const html = renderBoard(expanded);

    expect(html.match(/class="card__grip" draggable="true"/g)).toHaveLength(2);
    expect(html).toContain('aria-roledescription="拖拽手柄"');
    expect(html).toContain('aria-label="按住并拖动以调整卡片顺序"');
    // 正文、输入框、按钮都不再是拖拽源，只有把手能起拖。
    expect(html).not.toMatch(/<article[^>]*draggable/);
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

  // --- 翻译历史 ---

  it('offers a clock history button that reports its pressed state', () => {
    const closed = renderBoard(expanded)
      .match(/<button[^>]*data-action="history"[\s\S]*?<\/button>/)?.[0];
    expect(closed).toMatch(/^<button type="button"/);
    expect(closed).toContain('class="icon-btn icon-btn--history"');
    expect(closed).toContain('aria-pressed="false"');
    expect(closed).toContain('aria-label="翻译历史"');
    // 与其它头按钮同一套：16×16 viewBox + currentColor 描边时钟。
    expect(closed).toContain('viewBox="0 0 16 16"');
    expect(closed).toContain('stroke="currentColor"');
    expect(closed).toContain('<circle cx="8" cy="8"');

    const open = renderBoard({ ...expanded, historyOpen: true, historyItems: [] })
      .match(/<button[^>]*data-action="history"[\s\S]*?<\/button>/)?.[0];
    expect(open).toContain('aria-pressed="true"');
    expect(open).toContain('data-tooltip="关闭翻译历史"');
  });

  it('renders the MRU history list with snippet, target languages and local time', () => {
    const created = new Date(2026, 10, 7, 9, 30).getTime();
    const html = renderBoard({
      ...expanded,
      historyOpen: true,
      historyItems: [
        { id: 7, snippet: '你好世界', source_lang: 'zh', targets: ['en', 'ja'], created_at_ms: created },
        { id: 4, snippet: 'Good morning', source_lang: 'en', targets: ['zh'], created_at_ms: created },
      ],
    });

    expect(html).toContain('board__history');
    expect(html).not.toContain('board__cards');
    expect(html.match(/data-action="apply-history"/g)).toHaveLength(2);
    expect(html).toContain('data-id="7"');
    expect(html).toContain('你好世界');
    expect(html).toContain('English、日本語');
    expect(html).toContain('11-07 09:30');
    // MRU 顺序由 Rust 保证，渲染层不重排：第一条必须在前。
    expect(html.indexOf('data-id="7"')).toBeLessThan(html.indexOf('data-id="4"'));
  });

  it('shows an empty state and a clear entry when history is empty', () => {
    const html = renderBoard({ ...expanded, historyOpen: true, historyItems: [] });
    expect(html).toContain('history__empty');
    expect(html).toContain('还没有翻译历史');
    expect(html).toContain('data-action="clear-history"');
    expect(html.match(/data-action="apply-history"/g)).toBeNull();
  });

  it('escapes history snippets so clipboard text cannot inject markup', () => {
    const html = renderBoard({
      ...expanded,
      historyOpen: true,
      historyItems: [
        { id: 1, snippet: '<img src=x onerror=alert(1)>', targets: ['en'], created_at_ms: 0 },
      ],
    });
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
  });

  it('offers a clear-history entry in the settings section', () => {
    const html = renderBoard({ ...expanded, languagesOpen: true });
    expect(html).toContain('data-action="clear-history"');
  });
});
