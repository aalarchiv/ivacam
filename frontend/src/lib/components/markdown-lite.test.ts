import { describe, it, expect } from 'vitest';
import { renderMarkdown } from './markdown-lite';

describe('renderMarkdown (subset)', () => {
  it('renders headings at their level', () => {
    expect(renderMarkdown('### License')).toBe('<h3>License</h3>');
    expect(renderMarkdown('# Top')).toBe('<h1>Top</h1>');
  });

  it('wraps blank-line-separated paragraphs and joins wrapped lines', () => {
    expect(renderMarkdown('one\ntwo\n\nthree')).toBe('<p>one two</p>\n<p>three</p>');
  });

  it('renders bold, italic, and inline code', () => {
    expect(renderMarkdown('a **b** c')).toBe('<p>a <strong>b</strong> c</p>');
    expect(renderMarkdown('a *b* c')).toBe('<p>a <em>b</em> c</p>');
    expect(renderMarkdown('use `code` here')).toBe('<p>use <code>code</code> here</p>');
  });

  it('does not format inside code spans, and a bare number survives', () => {
    expect(renderMarkdown('`a *b* 3` then 3')).toBe('<p><code>a *b* 3</code> then 3</p>');
  });

  it('renders safe links and keeps bold around a link', () => {
    expect(renderMarkdown('[x](https://e.com)')).toBe(
      '<p><a href="https://e.com" target="_blank" rel="noreferrer">x</a></p>',
    );
    expect(renderMarkdown('**[x](https://e.com)**')).toBe(
      '<p><strong><a href="https://e.com" target="_blank" rel="noreferrer">x</a></strong></p>',
    );
  });

  it('drops the href of an unsafe link, keeping the label', () => {
    expect(renderMarkdown('[x](javascript:alert)')).toBe('<p>x</p>');
  });

  it('builds a bullet list', () => {
    expect(renderMarkdown('- one\n- two')).toBe('<ul><li>one</li><li>two</li></ul>');
  });

  it('escapes raw HTML so the source cannot inject markup', () => {
    expect(renderMarkdown('<script>alert(1)</script>')).toBe(
      '<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>',
    );
  });

  it('drops HTML comments', () => {
    expect(renderMarkdown('<!-- note -->\nvisible')).toBe('<p>visible</p>');
  });
});
