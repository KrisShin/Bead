import { describe, expect, it } from 'vitest';
import { stalePasteLangs } from './paste-media.js';

const card = (lang, text) => ({ lang, text });

describe('paste thumbnail ownership', () => {
  it('新内容替换后旧缩略图消失', () => {
    const entries = new Map([['zh', { text: '你好世界', pending: false }]]);
    expect(stalePasteLangs(entries, [card('zh', '新的文字'), card('en', 'new text')])).toEqual([
      'zh',
    ]);
  });

  it('清空同时移除文字与缩略图', () => {
    // clear_document 保留槽位但把文本置空：缩略图必须跟着文字一起清掉。
    const entries = new Map([['zh', { text: '你好世界', pending: false }]]);
    expect(stalePasteLangs(entries, [card('zh', '')])).toEqual(['zh']);
  });

  it('卡片不再是当前语言池成员时缩略图消失', () => {
    const entries = new Map([['zh', { text: '你好世界', pending: false }]]);
    expect(stalePasteLangs(entries, [card('en', '你好世界')])).toEqual(['zh']);
  });

  it('文本未变时缩略图保留', () => {
    const entries = new Map([['zh', { text: '你好世界', pending: false }]]);
    expect(stalePasteLangs(entries, [card('zh', '你好世界')])).toEqual([]);
  });

  it('识别中的缩略图不会被同期的翻译刷新误删', () => {
    const entries = new Map([['zh', { text: '旧文本', pending: true }]]);
    expect(stalePasteLangs(entries, [card('zh', '识别完成后才会写入的文本')])).toEqual([]);
  });
});
