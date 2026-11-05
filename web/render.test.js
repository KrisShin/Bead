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
});
