/// Minimal Markdown → HTML for TRUSTED, compiled-in copy — currently just
/// the About dialog's `ABOUT.md` (see `virtual:about` in vite.config.ts).
/// It exists so that prose can live in a `.md` without pulling in a full
/// markdown library.
///
/// Deliberate SUBSET only:
///   - `#`..`######` headings
///   - `**bold**`, `*italic*`, `` `code` ``
///   - `[label](href)` links (http/https/mailto only)
///   - `- ` / `* ` bullet lists
///   - blank-line-separated paragraphs
///   - `<!-- comments -->` are dropped
///
/// NOT a general renderer (no tables, blockquotes, images, nesting). The
/// source is HTML-escaped first, so a `.md` cannot inject markup — only the
/// whitelisted spans above become tags. Safe to feed to `{@html}` for the
/// trusted About copy; do NOT point it at untrusted input.

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

const SAFE_HREF = /^(?:https?:\/\/|mailto:)/i;

/// Inline spans over already-HTML-escaped text. Code spans are pulled out
/// first behind a `@@CODE<n>@@` placeholder (won't occur in real prose) so
/// their contents aren't re-formatted; then links, bold, italic are
/// applied; then code is restored.
function renderInline(text: string): string {
  const codes: string[] = [];
  let s = text.replace(/`([^`]+)`/g, (_m, c: string) => {
    codes.push(`<code>${c}</code>`);
    return `@@CODE${codes.length - 1}@@`;
  });
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, label: string, href: string) =>
    SAFE_HREF.test(href)
      ? `<a href="${href}" target="_blank" rel="noreferrer">${label}</a>`
      : label,
  );
  s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  s = s.replace(/(^|[^*])\*([^*\s][^*]*?)\*/g, '$1<em>$2</em>');
  s = s.replace(/@@CODE(\d+)@@/g, (_m, i: string) => codes[Number(i)]);
  return s;
}

/// Render the supported Markdown subset to an HTML string.
export function renderMarkdown(src: string): string {
  // Drop HTML comments up front (authoring notes in the .md).
  const escaped = escapeHtml(src.replace(/\r\n/g, '\n').replace(/<!--[\s\S]*?-->/g, ''));
  const lines = escaped.split('\n');
  const out: string[] = [];
  let para: string[] = [];
  let list: string[] = [];

  const flushPara = () => {
    if (para.length) {
      out.push(`<p>${renderInline(para.join(' '))}</p>`);
      para = [];
    }
  };
  const flushList = () => {
    if (list.length) {
      out.push(`<ul>${list.map((li) => `<li>${renderInline(li)}</li>`).join('')}</ul>`);
      list = [];
    }
  };

  for (const raw of lines) {
    const line = raw.replace(/\s+$/, '');
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    const bullet = /^[-*]\s+(.*)$/.exec(line);
    if (heading) {
      flushPara();
      flushList();
      const n = heading[1].length;
      out.push(`<h${n}>${renderInline(heading[2])}</h${n}>`);
    } else if (bullet) {
      flushPara();
      list.push(bullet[1]);
    } else if (line.trim() === '') {
      flushPara();
      flushList();
    } else {
      flushList();
      para.push(line);
    }
  }
  flushPara();
  flushList();
  return out.join('\n');
}
