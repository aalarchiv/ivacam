import { describe, it, expect } from 'vitest';
import { tooltipPosition } from './longpress-tooltip';

describe('tooltipPosition', () => {
  const tip = { width: 100, height: 30 };
  const viewport = { width: 400 };

  it('sits centred above the anchor when there is room', () => {
    const anchor = { top: 200, bottom: 220, left: 150, width: 40 };
    const { top, left } = tooltipPosition(anchor, tip, viewport);
    expect(top).toBe(200 - 30 - 8); // above, minus gap
    expect(left).toBe(150 + 20 - 50); // anchor centre minus half tip width
  });

  it('flips below the anchor when there is no room above', () => {
    const anchor = { top: 5, bottom: 30, left: 150, width: 40 };
    const { top } = tooltipPosition(anchor, tip, viewport);
    expect(top).toBe(30 + 8); // anchor bottom plus gap
  });

  it('clamps to the left margin', () => {
    const anchor = { top: 200, bottom: 220, left: 0, width: 10 };
    const { left } = tooltipPosition(anchor, tip, viewport);
    expect(left).toBe(4); // MARGIN_PX
  });

  it('clamps to the right margin', () => {
    const anchor = { top: 200, bottom: 220, left: 390, width: 10 };
    const { left } = tooltipPosition(anchor, tip, viewport);
    expect(left).toBe(400 - 100 - 4); // viewport width - tip width - margin
  });
});
