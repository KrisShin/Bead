import { describe, expect, it } from 'vitest';
import {
  canStartCardDrag,
  cardReorderDuration,
  insertionSide,
  orderChanged,
  planCardReorder,
} from './card-drag.js';

const order = ['zh', 'en', 'ja'];

describe('card drag reorder', () => {
  it('dragging a card across its neighbour drops it into the neighbour slot', () => {
    expect(planCardReorder(order, 'zh', 'en', 'after')).toEqual(['en', 'zh', 'ja']);
  });

  it('dragging a card left past its neighbour puts it in front', () => {
    expect(planCardReorder(order, 'ja', 'en', 'before')).toEqual(['zh', 'ja', 'en']);
  });

  it('dropping a card back where it started keeps the order and sends no reorder', () => {
    // en 本来就在 zh 后面：落在 zh 右半区＝原来的槽位，顺序不变。
    const next = planCardReorder(order, 'en', 'zh', 'after');

    expect(next).toEqual(order);
    expect(orderChanged(order, next)).toBe(false);
  });

  it('dropping a card onto itself does not change the order', () => {
    expect(planCardReorder(order, 'en', 'en', 'after')).toEqual(order);
  });

  it('a card being edited cannot start a drag', () => {
    expect(canStartCardDrag({ expanded: true, editing: true, onGrip: true })).toBe(false);
    expect(canStartCardDrag({ expanded: true, editing: false, onGrip: true })).toBe(true);
  });

  it('a drag only starts from the grip bar, not from the card body', () => {
    expect(canStartCardDrag({ expanded: true, editing: false, onGrip: false })).toBe(false);
    expect(canStartCardDrag({ expanded: true, editing: false, onGrip: true })).toBe(true);
  });

  it('a drag does not start while the panel is not settled or not expanded', () => {
    expect(canStartCardDrag({ expanded: true, onGrip: true, busy: true })).toBe(false);
    expect(canStartCardDrag({ expanded: false, onGrip: true })).toBe(false);
  });

  it('reordering is instant when reduced motion is requested', () => {
    expect(cardReorderDuration(true)).toBe(0);
    expect(cardReorderDuration(false)).toBe(160);
  });

  it('the left half of a card inserts before it and the right half after it', () => {
    const rect = { left: 100, width: 189 };

    expect(insertionSide(120, rect)).toBe('before');
    expect(insertionSide(250, rect)).toBe('after');
  });
});
