// Small, safe Markdown renderer for alert details. No dependencies.
//
// Safety: the output is built only with createElement / textContent, so no input
// is ever parsed as HTML (raw HTML in the source is shown as text). Links are
// kept only for http(s) URLs and open in a new tab without referrer. Images are
// never loaded: they render as links.
//
// Supported: ATX headings, paragraphs (single newlines become line breaks),
// **bold**, *italic*, ~~strike~~, `code`, fenced code blocks (``` or ~~~, with a
// language label), bullet and numbered lists (one nesting level), blockquotes,
// horizontal rules, GFM pipe tables with alignment, [links](https://…),
// <https://…> and bare https://… URLs.
'use strict';

window.InboxMarkdown = (() => {
  function node(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function safeUrl(raw) {
    try {
      const u = new URL(raw, location.href);
      return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null;
    } catch (_) {
      return null;
    }
  }

  function link(href, children) {
    const url = safeUrl(href);
    if (!url) return children;
    const a = node('a');
    a.href = url;
    a.target = '_blank';
    a.rel = 'noopener noreferrer';
    a.append(...children);
    return [a];
  }

  // ------------------------------------------------------------ inline

  const INLINE_SOURCE = [
    '(`+)([\\s\\S]*?[^`])\\1(?!`)', // 1,2 code span
    '!\\[([^\\]]*)\\]\\(\\s*([^)\\s]+)(?:\\s+"[^"]*")?\\s*\\)', // 3,4 image
    '\\[([^\\]]+)\\]\\(\\s*([^)\\s]+)(?:\\s+"[^"]*")?\\s*\\)', // 5,6 link
    '\\*\\*(?=\\S)([\\s\\S]*?\\S)\\*\\*', // 7 bold
    '__(?=\\S)([\\s\\S]*?\\S)__', // 8 bold
    '~~(?=\\S)([\\s\\S]*?\\S)~~', // 9 strike
    '\\*(?=[^\\s*])([^*]*?[^\\s*])\\*', // 10 italic
    '(?<![A-Za-z0-9_])_(?=[^\\s_])([^_]*?[^\\s_])_(?![A-Za-z0-9_])', // 11 italic
    '<(https?://[^>\\s]+)>', // 12 autolink
    '(https?://[^\\s<>]*[^\\s<>.,;:!?\'")\\]])', // 13 bare URL
  ].join('|');

  function inline(text) {
    // A fresh regex per call: inline() recurses into bold, italic and link text,
    // and a shared global regex would have its lastIndex reset by the inner call.
    const re = new RegExp(INLINE_SOURCE, 'g');
    const out = [];
    let last = 0;
    let m;
    while ((m = re.exec(text)) !== null) {
      if (m.index > last) out.push(document.createTextNode(text.slice(last, m.index)));
      last = re.lastIndex;
      if (m[2] !== undefined) out.push(node('code', null, m[2].replace(/^ (.*) $/, '$1')));
      else if (m[4] !== undefined) out.push(...link(m[4], [document.createTextNode(m[3] || m[4])]));
      else if (m[6] !== undefined) out.push(...link(m[6], inline(m[5])));
      else if (m[7] !== undefined || m[8] !== undefined) {
        const s = node('strong');
        s.append(...inline(m[7] !== undefined ? m[7] : m[8]));
        out.push(s);
      } else if (m[9] !== undefined) {
        const s = node('del');
        s.append(...inline(m[9]));
        out.push(s);
      } else if (m[10] !== undefined || m[11] !== undefined) {
        const s = node('em');
        s.append(...inline(m[10] !== undefined ? m[10] : m[11]));
        out.push(s);
      } else if (m[12] !== undefined) out.push(...link(m[12], [document.createTextNode(m[12])]));
      else if (m[13] !== undefined) out.push(...link(m[13], [document.createTextNode(m[13])]));
    }
    if (last < text.length) out.push(document.createTextNode(text.slice(last)));
    return out;
  }

  // Paragraph text: single newlines become <br>.
  function inlineLines(lines) {
    const out = [];
    lines.forEach((l, i) => {
      if (i > 0) out.push(node('br'));
      out.push(...inline(l));
    });
    return out;
  }

  // ------------------------------------------------------------ tables

  function splitRow(line) {
    let s = line.trim();
    if (s.startsWith('|')) s = s.slice(1);
    if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
    const cells = [];
    let cur = '';
    let inCode = false;
    for (let i = 0; i < s.length; i++) {
      const ch = s[i];
      if (ch === '\\' && s[i + 1] === '|') { cur += '|'; i++; continue; }
      if (ch === '`') inCode = !inCode;
      if (ch === '|' && !inCode) { cells.push(cur.trim()); cur = ''; continue; }
      cur += ch;
    }
    cells.push(cur.trim());
    return cells;
  }

  const isSeparator = (line) => /^\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/.test(line) && line.includes('-');

  function table(header, sep, rows) {
    const align = splitRow(sep).map((c) => (c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'right' : c.startsWith(':') ? 'left' : ''));
    const head = splitRow(header);
    const t = node('table');
    const thead = node('thead');
    const tr = node('tr');
    head.forEach((c, i) => {
      const th = node('th');
      if (align[i]) th.className = 'align-' + align[i];
      th.append(...inline(c));
      tr.append(th);
    });
    thead.append(tr);
    const tbody = node('tbody');
    for (const r of rows) {
      const cells = splitRow(r);
      const row = node('tr');
      for (let i = 0; i < head.length; i++) {
        const td = node('td');
        if (align[i]) td.className = 'align-' + align[i];
        td.append(...inline(cells[i] || ''));
        row.append(td);
      }
      tbody.append(row);
    }
    t.append(thead, tbody);
    const wrap = node('div', 'md-table');
    wrap.append(t);
    return wrap;
  }

  // ------------------------------------------------------------ blocks

  const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([\w#+.-]*)\s*$/;
  const HEADING = /^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
  const HR = /^ {0,3}([-*_])(\s*\1){2,}\s*$/;
  const QUOTE = /^ {0,3}>\s?(.*)$/;
  const ITEM = /^( *)([-*+]|\d{1,9}[.)])\s+(.*)$/;

  function blocks(lines) {
    const out = [];
    let i = 0;
    let para = [];
    const flush = () => {
      if (!para.length) return;
      const p = node('p');
      p.append(...inlineLines(para));
      out.push(p);
      para = [];
    };

    while (i < lines.length) {
      const line = lines[i];

      const fence = line.match(FENCE);
      if (fence) {
        flush();
        const marker = fence[1];
        const body = [];
        i++;
        while (i < lines.length && !new RegExp('^ {0,3}' + marker[0] + '{' + marker.length + ',}\\s*$').test(lines[i])) body.push(lines[i++]);
        i++;
        const wrap = node('div', 'md-code');
        if (fence[2]) wrap.append(node('span', 'md-lang', fence[2]));
        const pre = node('pre');
        pre.append(node('code', null, body.join('\n')));
        wrap.append(pre);
        out.push(wrap);
        continue;
      }

      if (!line.trim()) { flush(); i++; continue; }

      const h = line.match(HEADING);
      if (h) {
        flush();
        const e = node('h' + Math.min(h[1].length + 2, 6), 'md-h' + h[1].length);
        e.append(...inline(h[2]));
        out.push(e);
        i++;
        continue;
      }

      if (HR.test(line) && !(para.length && /^ {0,3}-+\s*$/.test(line))) {
        flush();
        out.push(node('hr'));
        i++;
        continue;
      }

      if (line.includes('|') && i + 1 < lines.length && isSeparator(lines[i + 1]) && splitRow(line).length > 1) {
        flush();
        const header = line;
        const sep = lines[i + 1];
        i += 2;
        const rows = [];
        while (i < lines.length && lines[i].trim() && lines[i].includes('|')) rows.push(lines[i++]);
        out.push(table(header, sep, rows));
        continue;
      }

      if (QUOTE.test(line)) {
        flush();
        const inner = [];
        while (i < lines.length && QUOTE.test(lines[i])) inner.push(lines[i++].match(QUOTE)[1]);
        const q = node('blockquote');
        q.append(...blocks(inner));
        out.push(q);
        continue;
      }

      if (ITEM.test(line)) {
        flush();
        const [list, next] = parseList(lines, i);
        out.push(list);
        i = next;
        continue;
      }

      para.push(line.trim());
      i++;
    }
    flush();
    return out;
  }

  // One list starting at `start`; items indented by 2+ spaces under an item form
  // a nested list (one level).
  function parseList(lines, start) {
    const first = lines[start].match(ITEM);
    const baseIndent = first[1].length;
    const ordered = /\d/.test(first[2]);
    const list = node(ordered ? 'ol' : 'ul');
    if (ordered) {
      const n = parseInt(first[2], 10);
      if (n !== 1) list.start = n;
    }
    let i = start;
    let item = null;
    let itemLines = [];
    const closeItem = () => {
      if (!item) return;
      item.append(...inlineLines(itemLines));
      itemLines = [];
    };
    while (i < lines.length) {
      const line = lines[i];
      if (!line.trim()) {
        // A blank line ends the list unless the next line continues it.
        const nx = lines[i + 1];
        const m = nx && nx.match(ITEM);
        if (m && m[1].length >= baseIndent) { i++; continue; }
        break;
      }
      const m = line.match(ITEM);
      if (m && m[1].length <= baseIndent + 1) {
        if (/\d/.test(m[2]) !== ordered) break;
        closeItem();
        item = node('li');
        list.append(item);
        itemLines = [m[3]];
        i++;
        continue;
      }
      if (m && m[1].length >= baseIndent + 2 && item) {
        // Nested list: collect its lines (deeper levels are flattened into it).
        closeItem();
        const nested = [];
        while (i < lines.length) {
          const n = lines[i].match(ITEM);
          if (n && n[1].length >= baseIndent + 2) nested.push(lines[i].slice(baseIndent + 2).replace(/^\s+(?=([-*+]|\d{1,9}[.)])\s)/, ''));
          else if (lines[i].trim() && !n && /^\s{2,}/.test(lines[i]) && nested.length) nested[nested.length - 1] += ' ' + lines[i].trim();
          else break;
          i++;
        }
        const [sub] = parseList(nested.map((l) => l.replace(/^\s+/, '')), 0);
        item.append(sub);
        continue;
      }
      if (item && /^\s+\S/.test(line) && !ITEM.test(line)) {
        itemLines.push(line.trim());
        i++;
        continue;
      }
      if (item && !ITEM.test(line) && !FENCE.test(line) && !HEADING.test(line) && !QUOTE.test(line)) {
        // Lazy continuation of the last item.
        itemLines.push(line.trim());
        i++;
        continue;
      }
      break;
    }
    closeItem();
    return [list, i];
  }

  /** Renders `source` into a fresh `<div class="md">`. */
  function render(source) {
    const root = node('div', 'md');
    const text = String(source || '').replace(/\r\n?/g, '\n').replace(/\t/g, '    ');
    root.append(...blocks(text.split('\n')));
    return root;
  }

  /** One-line plain-text preview of `source` (for dense rows). */
  function plain(source) {
    return String(source || '')
      .replace(/\r\n?/g, '\n')
      .replace(/^ {0,3}(`{3,}|~{3,}).*$/gm, '')
      .replace(/^(?=[\s|:-]*-)[\s|:-]+$/gm, '')
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
      .replace(/^ {0,3}(#{1,6}|>|[-*+]|\d+[.)])\s+/gm, '')
      .replace(/(\*\*|__|~~|`|\*)/g, '')
      .replace(/\\\|/g, '|')
      .replace(/\|/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
  }

  return { render, plain };
})();
