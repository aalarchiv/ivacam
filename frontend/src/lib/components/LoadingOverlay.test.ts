/// Logic-level coverage for LoadingOverlay's pure helpers. The vitest
/// config runs without the Svelte plugin (see vitest.config.ts), so we
/// exercise the message/visibility helpers directly.

import { describe, expect, it } from 'vitest';
import { loadingMessage, shouldShow } from './loading_overlay';

describe('loadingMessage', () => {
  it('returns a default when the input is nullish', () => {
    expect(loadingMessage(null)).toBe('Loading…');
    expect(loadingMessage(undefined)).toBe('Loading…');
  });

  it('returns a default when the input is empty / whitespace', () => {
    expect(loadingMessage('')).toBe('Loading…');
    expect(loadingMessage('   ')).toBe('Loading…');
  });

  it('returns the trimmed input when non-empty', () => {
    expect(loadingMessage('Parsing DXF…')).toBe('Parsing DXF…');
    expect(loadingMessage('  Loading project…  ')).toBe('Loading project…');
  });
});

describe('shouldShow', () => {
  it('returns true only when visible is exactly true', () => {
    expect(shouldShow(true)).toBe(true);
    expect(shouldShow(false)).toBe(false);
  });
});
