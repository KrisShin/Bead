import { describe, expect, it } from 'vitest';
import { escapeHtml, renderBoard } from './render.js';

const expanded = {
  view: 'expanded',
  locked: false,
  notice: null,
  cards: [
    { lang: 'zh', text: '你好', status: 'idle', authoritative: true },
    { lang: 'en', text: 'Hello', status: 'idle', authoritative: false },
  ],
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

  it('returns only the capsule when the state is not expanded', () => {
    const html = renderBoard({ ...expanded, view: 'capsule' });
    expect(html).toContain('id="capsule"');
    expect(html).not.toContain('id="board"');
  });

  it('shows the clipboard empty notice without an error state', () => {
    const html = renderBoard({ ...expanded, notice: '剪贴板为空或非文本' });
    expect(html).toContain('剪贴板为空或非文本');
  });

  it('renders each card text as an editable textarea carrying the current text', () => {
    const html = renderBoard(expanded);
    expect(html.match(/<textarea class="card__text"/g)).toHaveLength(2);
    expect(html).toContain('>你好</textarea>');
  });

  it('offers a retry button only on the errored card', () => {
    const html = renderBoard({
      ...expanded,
      cards: [expanded.cards[0], { ...expanded.cards[1], status: 'error' }],
    });
    expect(html.match(/data-action="retry"/g)).toHaveLength(1);
    expect(html).toContain('重试');
  });

  it('reveals the language panel with all candidates only when opened', () => {
    expect(renderBoard(expanded)).not.toContain('board__langs');
    const html = renderBoard({ ...expanded, languagesOpen: true });
    expect(html).toContain('board__langs');
    expect(html).toContain('data-action="languages"');
    expect(html.match(/data-lang-code="/g)).toHaveLength(8);
  });

  it('checks pool members and locks changes that would break the two-to-five bound', () => {
    const two = renderBoard({ ...expanded, languagesOpen: true });
    // 2 张卡：移除任一都会跌破下限，被锁；新增第三种不受限
    expect(two).toContain('<input type="checkbox" data-lang-code="zh" checked disabled />');
    expect(two).toContain('<input type="checkbox" data-lang-code="en" checked disabled />');
    expect(two).toContain('<input type="checkbox" data-lang-code="ja" />');

    const five = renderBoard({
      ...expanded,
      languagesOpen: true,
      cards: ['zh', 'en', 'ja', 'ko', 'fr'].map((lang) => ({
        lang,
        text: 'x',
        status: 'idle',
        authoritative: lang === 'zh',
      })),
    });
    // 5 张卡：移除不受限，新增被锁
    expect(five).toContain('<input type="checkbox" data-lang-code="ja" checked />');
    expect(five).toContain('<input type="checkbox" data-lang-code="de" disabled />');
  });

  it('renders the provider panel with saved credentials never echoed back', () => {
    const html = renderBoard({
      ...expanded,
      languagesOpen: true,
      provider: {
        engine: 'zhipu',
        has_key: true,
        has_secret: false,
        needs_secret: false,
        model: 'glm-4.7-flash',
        base_url: '',
      },
    });
    expect(html).toContain('data-provider="engine"');
    expect(html).toContain('value="zhipu" selected');
    expect(html).toContain('type="password"');
    expect(html).toContain('已保存（留空保持）');
    expect(html.match(/value="glm-4\.7-flash"/g)).toHaveLength(1); // 模型可回显，密钥绝不
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

  it('gives every card a speak entry next to copy', () => {
    const html = renderBoard(expanded);
    expect(html.match(/data-action="speak"/g)).toHaveLength(2);
    expect(html).toContain('朗读');
  });
});
