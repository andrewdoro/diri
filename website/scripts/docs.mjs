// Builds diri.sh/docs from docs-src/*.md: one static page per doc, raw Markdown
// beside it for agents, a search index, and llms.txt. The output is committed so
// the site still needs no build step to preview. `--check` fails when it is stale.
import { readFile, writeFile, mkdir, readdir, rm } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const source = resolve(root, 'docs-src');
const site = 'https://diri.sh';
const repo = 'https://github.com/cristicretu/diri';
const today = '2026-10-01';

// The sidebar. Every docs-src page must appear here exactly once.
export const NAV = [
  { group: 'Getting started', pages: [
    ['index', 'file'], ['install', 'download'], ['quickstart', 'new-agent'], ['keyboard-shortcuts', 'keyboard'],
  ] },
  { group: 'Working with agents', pages: [
    ['sessions', 'terminal'], ['worktrees', 'branch'], ['notes', 'note'], ['scheduled-tasks', 'clock'], ['accounts', 'user'],
  ] },
  { group: 'Automation', pages: [
    ['mcp', 'plug'], ['mcp-tools', 'code'], ['cli', 'terminal'], ['agents', 'apps'],
  ] },
  { group: 'Remote', pages: [
    ['remote-hosts', 'server'],
  ] },
  { group: 'Reference', pages: [
    ['security', 'shield'], ['troubleshooting', 'lifebuoy'],
  ] },
];

const esc = text => text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const slugify = text => text.toLowerCase().replace(/<[^>]+>/g, '').replace(/[`*]/g, '').replace(/&[a-z]+;/g, '').replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
const pageUrl = slug => slug === 'index' ? '/docs/' : `/docs/${slug}/`;
const plain = md => md.replace(/<\/?kbd>/g, '').replace(/!\[[^\]]*\]\([^)]*\)/g, '').replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').replace(/[`*]/g, '').replace(/\s+/g, ' ').trim();

// ── Inline Markdown ─────────────────────────────────────────────────────────
function inline(text) {
  const held = [];
  const hold = html => `\u0000${held.push(html) - 1}\u0000`;
  text = text.replace(/`([^`]+)`/g, (_, code) => hold(`<code>${esc(code)}</code>`));
  text = text.replace(/<kbd>(.*?)<\/kbd>/g, (_, key) => hold(`<kbd>${esc(key)}</kbd>`));
  text = esc(text);
  text = text.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, href) => {
    const external = /^https?:/.test(href) && !href.startsWith(site);
    return hold(`<a href="${href}"${external ? ' rel="noopener"' : ''}>${label}</a>`);
  });
  text = text.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  text = text.replace(/(^|[\s(])\*([^*\s][^*]*?)\*(?=[\s).,;:!?]|$)/g, '$1<em>$2</em>');
  return text.replace(/\u0000(\d+)\u0000/g, (_, i) => held[i]).replace(/\u0000(\d+)\u0000/g, (_, i) => held[i]);
}

// ── Code: a few colours for the languages the docs use ──────────────────────
function highlight(code, lang) {
  if (lang === 'json' || lang === 'jsonc') {
    return esc(code).replace(/(&quot;(?:[^&]|&(?!quot;))*?&quot;)(\s*:)?|\b(true|false|null)\b|(-?\b\d+(?:\.\d+)?\b)/g, (m, str, colon, lit, num) =>
      str ? `<span class="${colon ? 'tk-key' : 'tk-str'}">${str}</span>${colon || ''}` : lit ? `<span class="tk-lit">${lit}</span>` : `<span class="tk-num">${num}</span>`);
  }
  if (['sh', 'fish', 'bash', 'shell', 'console'].includes(lang)) {
    return code.split('\n').map(line => {
      if (/^\s*#/.test(line)) return `<span class="tk-comment">${esc(line)}</span>`;
      const m = line.match(/^(\s*(?:\$\s+)?)([\w./-]+)(.*)$/);
      if (!m) return esc(line);
      const rest = esc(m[3]).replace(/(&quot;.*?&quot;|'[^']*')/g, '<span class="tk-str">$1</span>').replace(/(\s)(--?[\w-]+)/g, '$1<span class="tk-flag">$2</span>');
      return `${esc(m[1])}<span class="tk-cmd">${esc(m[2])}</span>${rest}`;
    }).join('\n');
  }
  return esc(code);
}

// ── Images: read intrinsic sizes so pages do not shift as they load ─────────
async function imageSize(src) {
  try {
    const bytes = await readFile(resolve(root, '.' + src));
    if (bytes.toString('ascii', 1, 4) === 'PNG') return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
    if (bytes.toString('ascii', 8, 12) === 'WEBP') {
      const kind = bytes.toString('ascii', 12, 16);
      if (kind === 'VP8X') return [1 + bytes.readUIntLE(24, 3), 1 + bytes.readUIntLE(27, 3)];
      if (kind === 'VP8L') { const b = bytes.readUInt32LE(21); return [1 + (b & 0x3fff), 1 + ((b >> 14) & 0x3fff)]; }
      if (kind === 'VP8 ') return [bytes.readUInt16LE(26) & 0x3fff, bytes.readUInt16LE(28) & 0x3fff];
    }
  } catch {}
  return null;
}

// ── Blocks ──────────────────────────────────────────────────────────────────
async function render(md) {
  const lines = md.replace(/\r/g, '').split('\n');
  const out = [];
  const toc = [];
  const ids = new Set();
  const uniqueId = text => { let id = slugify(text) || 'section'; let n = 2; while (ids.has(id)) id = `${slugify(text)}-${n++}`; ids.add(id); return id; };
  let i = 0;
  const isBlockStart = line => /^(#{2,4} |```|> |\s*[-*] |\s*\d+\. |\||!\[)/.test(line) || line.trim() === '';
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }
    let m;
    if ((m = line.match(/^(#{2,4}) (.+)$/))) {
      const level = m[1].length;
      const id = uniqueId(m[2]);
      if (level === 2) toc.push({ id, text: plain(m[2]) });
      out.push(`<h${level} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true" tabindex="-1">#</a>${inline(m[2])}</h${level}>`);
      i++; continue;
    }
    if ((m = line.match(/^```(\S*)\s*(.*)$/))) {
      const lang = m[1];
      const title = m[2];
      const body = [];
      i++;
      while (i < lines.length && !lines[i].startsWith('```')) body.push(lines[i++]);
      i++;
      const code = body.join('\n');
      const label = title || { sh: 'Terminal', fish: 'Terminal', bash: 'Terminal', json: 'JSON', toml: 'TOML', text: '', jsonc: 'JSON' }[lang] || lang;
      out.push(`<div class="code-block"${lang ? ` data-lang="${esc(lang)}"` : ''}>${label ? `<div class="code-head"><span>${esc(label)}</span></div>` : ''}<button class="code-copy" type="button" aria-label="Copy code"><span class="icon" aria-hidden="true"></span></button><pre><code>${highlight(code, lang)}</code></pre></div>`);
      continue;
    }
    if ((m = line.match(/^> \[!(NOTE|TIP|WARNING)\]\s*$/))) {
      const kind = m[1].toLowerCase();
      const body = [];
      i++;
      while (i < lines.length && lines[i].startsWith('>')) body.push(lines[i++].replace(/^>\s?/, ''));
      const label = { note: 'Note', tip: 'Tip', warning: 'Warning' }[kind];
      out.push(`<aside class="callout ${kind}"><strong>${label}</strong>${(await render(body.join('\n'))).html}</aside>`);
      continue;
    }
    if (line.startsWith('> ')) {
      const body = [];
      while (i < lines.length && lines[i].startsWith('>')) body.push(lines[i++].replace(/^>\s?/, ''));
      out.push(`<blockquote>${(await render(body.join('\n'))).html}</blockquote>`);
      continue;
    }
    if ((m = line.match(/^!\[([^\]]*)\]\(([^)\s]+)\)\s*$/))) {
      const size = await imageSize(m[2]);
      const dims = size ? ` width="${size[0]}" height="${size[1]}"` : '';
      out.push(`<figure class="doc-shot"><img src="${m[2]}"${dims} alt="${esc(m[1])}" loading="lazy" decoding="async">${m[1] ? `<figcaption>${inline(m[1])}</figcaption>` : ''}</figure>`);
      i++; continue;
    }
    if (line.startsWith('|')) {
      const rows = [];
      while (i < lines.length && lines[i].startsWith('|')) rows.push(lines[i++]);
      const cells = row => row.trim().replace(/^\||\|$/g, '').split(/(?<!\\)\|/).map(cell => cell.trim().replace(/\\\|/g, '|'));
      const head = cells(rows[0]);
      const align = cells(rows[1]).map(c => c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'right' : '');
      const td = (tag, c, j) => `<${tag}${align[j] ? ` style="text-align:${align[j]}"` : ''}>${inline(c)}</${tag}>`;
      out.push(`<div class="table-wrap"><table><thead><tr>${head.map((c, j) => td('th', c, j)).join('')}</tr></thead><tbody>${rows.slice(2).map(r => `<tr>${cells(r).map((c, j) => td('td', c, j)).join('')}</tr>`).join('')}</tbody></table></div>`);
      continue;
    }
    if ((m = line.match(/^\s*([-*]|\d+\.) /))) {
      const ordered = /\d/.test(m[1]);
      const items = [];
      while (i < lines.length) {
        const item = lines[i].match(/^\s*([-*]|\d+\.) (.*)$/);
        if (item && /\d/.test(item[1]) === ordered) { items.push(item[2]); i++; continue; }
        if (lines[i].trim() && /^\s{2,}\S/.test(lines[i]) && items.length) { items[items.length - 1] += ' ' + lines[i].trim(); i++; continue; }
        break;
      }
      const tag = ordered ? 'ol' : 'ul';
      const start = ordered ? parseInt(m[1], 10) : 1;
      out.push(`<${tag}${start > 1 ? ` start="${start}"` : ''}>${items.map(item => `<li>${inline(item)}</li>`).join('')}</${tag}>`);
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !(para.length && isBlockStart(lines[i]))) para.push(lines[i++].trim());
    out.push(`<p>${inline(para.join(' '))}</p>`);
  }
  return { html: out.join('\n'), toc };
}

function frontmatter(text, file) {
  const m = text.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!m) throw new Error(`${file}: missing frontmatter`);
  const meta = Object.fromEntries(m[1].split('\n').filter(Boolean).map(line => {
    const at = line.indexOf(':');
    return [line.slice(0, at).trim(), line.slice(at + 1).trim().replace(/^"(.*)"$/, '$1')];
  }));
  if (!meta.title || !meta.description) throw new Error(`${file}: title and description are required`);
  if (meta.description.length < 80 || meta.description.length > 180) throw new Error(`${file}: description must be 80–180 characters (${meta.description.length})`);
  return { meta, body: text.slice(m[0].length).trim() + '\n' };
}

// ── The MCP tool reference is generated from the Rust catalog ───────────────
const TOOL_GROUPS = [
  ['Start and manage agents', ['spawn_agent', 'spawn_agents', 'fork_agent', 'manage_agent', 'release_agent', 'list_agents', 'get_status']],
  ['Talk to agents and wait', ['send_prompt', 'wait_any', 'wait_for_agent', 'read_output']],
  ['Tracked tasks', ['submit_task', 'submit_tasks', 'get_task', 'wait_for_task', 'report_task', 'answer_task', 'cancel_task', 'list_tasks']],
  ['Lineage', ['whoami', 'list_children', 'wait_for_children', 'summarize_children', 'report_to_parent']],
  ['Code and worktrees', ['get_diff', 'integrate', 'create_worktree', 'list_worktrees', 'remove_worktree', 'get_artifacts', 'quick_open_include']],
  ['Browser', ['browser', 'test_run']],
  ['Notes', ['list_notes', 'read_note', 'write_note', 'edit_note', 'replace_section', 'create_note', 'start_from_note', 'note_history']],
];

function schemaType(schema = {}) {
  if (schema.enum?.includes('shell') && schema.enum.length > 6) return `agent label or \`shell\` ([list](/docs/agents/))`;
  if (schema.enum) return schema.enum.map(v => `\`${v}\``).join(', ');
  if (schema.type === 'array') return `${schemaType(schema.items)} array`.replace(/^object array$/, 'array of objects');
  return schema.type || 'any';
}

function toolMarkdown(catalog) {
  const byName = new Map(catalog.tools.map(tool => [tool.name, tool]));
  const placed = new Set();
  const groups = TOOL_GROUPS.map(([name, list]) => [name, list.filter(n => byName.has(n))]);
  for (const [, list] of groups) list.forEach(n => placed.add(n));
  const rest = catalog.tools.map(t => t.name).filter(n => !placed.has(n));
  if (rest.length) groups.push(['Other', rest]);
  let md = '';
  for (const [group, names] of groups) {
    if (!names.length) continue;
    md += `\n## ${group}\n\n`;
    for (const name of names) {
      const tool = byName.get(name);
      md += `### \`${name}\`\n\n${tool.description.replace(/\|/g, '/')}\n\n`;
      const props = Object.entries(tool.inputSchema?.properties || {});
      const required = new Set(tool.inputSchema?.required || []);
      if (!props.length) { md += 'No arguments.\n\n'; continue; }
      md += '| Argument | Type | Notes |\n| --- | --- | --- |\n';
      props.sort(([a], [b]) => Number(required.has(b)) - Number(required.has(a)));
      for (const [arg, schema] of props) {
        const notes = [required.has(arg) ? '**Required.**' : '', schema.description || '', schema.default !== undefined ? `Default \`${JSON.stringify(schema.default)}\`.` : '',
          schema.items?.properties ? `Each item: ${Object.keys(schema.items.properties).map(k => `\`${k}\``).join(', ')}.` : ''].filter(Boolean).join(' ');
        md += `| \`${arg}\` | ${schemaType(schema)} | ${notes.replace(/\|/g, '/')} |\n`;
      }
      md += '\n';
    }
  }
  return md;
}

// ── Page chrome ─────────────────────────────────────────────────────────────
const header = `<header class="site-header">
      <a class="wordmark" href="/" aria-label="Diri home"><img class="app-mark" src="/assets/brand/diri-icon.svg" width="26" height="26" alt="">diri</a>
      <nav class="site-nav" aria-label="Main"><a href="/#features">Features</a><a href="/guides/">Guides</a><a href="/docs/" aria-current="page">Docs</a></nav>
      <div class="header-actions"><a class="github-link" href="${repo}" aria-label="Diri on GitHub" title="Diri on GitHub"><span class="icon" data-icon="github" aria-hidden="true"></span><span class="star-count" aria-hidden="true"></span></a><a class="nav-download" href="${repo}/releases/latest">Download</a></div>
    </header>`;
const footer = `<footer class="site-footer">
      <div class="footer-main">
        <div class="footer-about">
          <a class="footer-brand" href="/" aria-label="Diri home"><img class="app-mark" src="/assets/brand/diri-icon.svg" width="26" height="26" alt="">diri</a>
          <p>The best way to work with coding agents.</p>
        </div>
        <nav class="footer-columns" aria-label="Resources">
          <div><h3>Product</h3><a href="${repo}/releases/latest">Download</a><a href="/whats-new/">What's new</a><a href="${repo}/releases">Release notes</a><a href="/guides/">Guides</a></div>
          <div><h3>Project</h3><a href="${repo}">GitHub</a><a href="/docs/">Documentation</a><a href="${repo}/blob/main/ROADMAP.md">Roadmap</a></div>
          <div><h3>Legal</h3><a href="${repo}/blob/main/LICENSE">Apache 2.0</a><a href="${repo}/blob/main/PRIVACY.md">Privacy</a><a href="${repo}/blob/main/SECURITY.md">Security</a></div>
        </nav>
      </div>
    </footer>`;

function sidebar(current, pages) {
  const groups = NAV.map(({ group, pages: list }) => `<div class="docs-group"><div class="docs-group-head"><span class="icon" data-icon="chevron-down" aria-hidden="true"></span>${esc(group)}</div><ul>${list.map(([slug, icon]) => {
    const page = pages.get(slug);
    if (!page) return '';
    return `<li><a class="docs-row" href="${pageUrl(slug)}"${slug === current ? ' aria-current="page"' : ''}><span class="icon" data-icon="${icon}" aria-hidden="true"></span><span>${esc(page.meta.nav || page.meta.title)}</span></a></li>`;
  }).join('')}</ul></div>`).join('');
  return `<nav class="docs-sidebar" id="docs-sidebar" aria-label="Documentation">
          <button class="docs-search" type="button" data-open-search><span class="icon" data-icon="search" aria-hidden="true"></span><span>Search docs</span><kbd>⌘K</kbd></button>
          ${groups}
          <div class="docs-sidebar-foot"><a href="/llms.txt"><span class="icon" data-icon="markdown" aria-hidden="true"></span>llms.txt</a><a href="/docs/mcp/#docs-mcp"><span class="icon" data-icon="plug" aria-hidden="true"></span>Docs MCP</a></div>
        </nav>`;
}

function template({ slug, meta, html, toc, group, prev, next, pages }) {
  const url = site + pageUrl(slug);
  const title = slug === 'index' ? 'Documentation · Diri' : `${meta.title} · Diri docs`;
  const ld = JSON.stringify({ '@context': 'https://schema.org', '@type': 'TechArticle', headline: meta.title, description: meta.description, url, inLanguage: 'en', dateModified: today, publisher: { '@type': 'Organization', name: 'Diri', url: site + '/' }, isPartOf: { '@type': 'WebSite', name: 'Diri', url: site + '/' } });
  const raw = slug === 'index' ? '/docs/index.md' : `/docs/${slug}.md`;
  const ask = `https://claude.ai/new?q=${encodeURIComponent(`Read ${site}${raw} and help me with diri.`)}`;
  const tocHtml = toc.length > 1 ? `<span>On this page</span>${toc.map(t => `<a href="#${t.id}">${esc(t.text)}</a>`).join('')}` : '';
  const pager = `<nav class="docs-pager" aria-label="Previous and next">${prev ? `<a class="prev" href="${pageUrl(prev)}"><span>Previous</span>${esc(pages.get(prev).meta.title)}</a>` : '<span></span>'}${next ? `<a class="next" href="${pageUrl(next)}"><span>Next</span>${esc(pages.get(next).meta.title)}</a>` : '<span></span>'}</nav>`;
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="theme-color" content="#191724">
  <title>${esc(title)}</title>
  <meta name="description" content="${esc(meta.description)}">
  <meta name="robots" content="index, follow, max-image-preview:large">
  <link rel="canonical" href="${url}">
  <link rel="alternate" type="text/markdown" href="${raw}">
  <meta property="og:type" content="article">
  <meta property="og:site_name" content="Diri">
  <meta property="og:title" content="${esc(meta.title)}">
  <meta property="og:description" content="${esc(meta.description)}">
  <meta property="og:url" content="${url}">
  <meta property="og:image" content="${site}/assets/social-card.jpg">
  <meta property="og:image:alt" content="Diri: the best way to work with coding agents.">
  <meta name="twitter:card" content="summary_large_image">
  <link rel="icon" href="/favicon.svg" type="image/svg+xml">
  <link rel="stylesheet" href="/style.css">
  <link rel="stylesheet" href="/guides.css">
  <link rel="stylesheet" href="/docs.css">
  <script src="/docs.js" defer></script>
  <script type="application/ld+json">${ld}</script>
  <script type="module" src="/stats.js"></script>
</head>
<body class="guide-page docs-page">
  <a class="skip-link" href="#main">Skip to content</a>
  <div class="site-wrap">
    ${header}
    <div class="docs-shell">
      <button class="docs-menu" type="button" aria-expanded="false" aria-controls="docs-sidebar"><span class="icon" data-icon="sidebar" aria-hidden="true"></span>${esc(group)}<span class="docs-menu-sep">/</span><strong>${esc(meta.nav || meta.title)}</strong></button>
      ${sidebar(slug, pages)}
      <main id="main" class="docs-main" data-raw="${raw}">
        <header class="docs-heading"><span class="eyebrow">${esc(group)}</span><h1>${esc(meta.title)}</h1><p>${inline(meta.lead || meta.description)}</p></header>
        <article class="docs-content">
${html}
        </article>
        ${pager}
      </main>
      <aside class="docs-rail" aria-label="On this page">
        <div class="docs-rail-inner">${tocHtml ? `<nav class="docs-toc">${tocHtml}</nav>` : ''}
          <div class="docs-actions"><button type="button" data-copy-page><span class="icon" data-icon="copy" aria-hidden="true"></span><span>Copy as Markdown</span></button><a href="${raw}"><span class="icon" data-icon="markdown" aria-hidden="true"></span>View Markdown</a><a href="${ask}" rel="noopener"><span class="icon" data-icon="external-link" aria-hidden="true"></span>Ask Claude</a><a href="${repo}/edit/main/website/docs-src/${slug}.md" rel="noopener"><span class="icon" data-icon="github" aria-hidden="true"></span>Edit on GitHub</a></div>
        </div>
      </aside>
    </div>
    ${footer}
  </div>
  <div class="docs-search-dialog" hidden>
    <div class="docs-search-scrim" data-close-search></div>
    <div class="docs-search-panel" role="dialog" aria-modal="true" aria-label="Search documentation">
      <label class="docs-search-input"><span class="icon" data-icon="search" aria-hidden="true"></span><input type="search" placeholder="Search the docs" autocomplete="off" spellcheck="false" aria-controls="docs-search-results"><kbd>esc</kbd></label>
      <ul id="docs-search-results" class="docs-search-results" role="listbox"></ul>
      <div class="docs-search-foot"><span><kbd>↑</kbd><kbd>↓</kbd> to move</span><span><kbd>↵</kbd> to open</span></div>
    </div>
  </div>
</body>
</html>
`;
}

// ── Build ───────────────────────────────────────────────────────────────────
export async function buildDocs() {
  const files = (await readdir(source)).filter(f => f.endsWith('.md'));
  const pages = new Map();
  for (const file of files) {
    const slug = file.replace(/\.md$/, '');
    const { meta, body } = frontmatter(await readFile(resolve(source, file), 'utf8'), file);
    let markdown = body;
    if (markdown.includes('{{mcp-tools}}')) {
      const catalog = JSON.parse(await readFile(resolve(source, 'mcp-tools.json'), 'utf8'));
      markdown = markdown.replace('{{mcp-tools}}', toolMarkdown(catalog).trim());
    }
    pages.set(slug, { meta, markdown });
  }
  // DOCS_DRAFT=1 previews while some pages are still being written.
  const order = NAV.flatMap(({ group, pages: list }) => list.map(([slug]) => ({ slug, group }))).filter(({ slug }) => !process.env.DOCS_DRAFT || pages.has(slug));
  for (const { slug } of order) if (!pages.has(slug)) throw new Error(`docs-src/${slug}.md is in NAV but missing`);
  for (const slug of pages.keys()) if (!order.some(o => o.slug === slug)) throw new Error(`docs-src/${slug}.md is not in NAV`);

  const outputs = new Map();
  const search = [];
  const llms = [`# Diri\n\n> Diri is a native desktop app for running many coding agents (Claude Code, Codex, Cursor, Gemini and 16 more) side by side, with live status, git worktrees, review, notes, remote SSH hosts, and an MCP server that lets agents start and coordinate other agents.\n\nEach page is available as Markdown at the URL below. The full set is concatenated in ${site}/llms-full.txt. A read-only docs MCP server is at ${site}/mcp.\n`];
  let full = '';
  let lastGroup = '';
  for (const [n, { slug, group }] of order.entries()) {
    const { meta, markdown } = pages.get(slug);
    const { html, toc } = await render(markdown);
    const prev = order[n - 1]?.slug;
    const next = order[n + 1]?.slug;
    const path = slug === 'index' ? 'docs/index.html' : `docs/${slug}/index.html`;
    outputs.set(path, template({ slug, meta, html, toc, group, prev, next, pages }));
    const rawMd = `# ${meta.title}\n\n> ${meta.description}\n\n${markdown}`;
    outputs.set(slug === 'index' ? 'docs/index.md' : `docs/${slug}.md`, rawMd);
    full += `\n\n---\n\nSource: ${site}${pageUrl(slug)}\n\n${rawMd}`;
    if (group !== lastGroup) { llms.push(`\n## ${group}\n`); lastGroup = group; }
    llms.push(`- [${meta.title}](${site}${slug === 'index' ? '/docs/index.md' : `/docs/${slug}.md`}): ${meta.description}`);

    // Search: the page itself, then each section with its first words.
    search.push({ t: meta.title, g: group, u: pageUrl(slug), x: meta.description });
    const sections = markdown.split(/^(?=#{2,3} )/m);
    for (const section of sections) {
      const head = section.match(/^(#{2,3}) (.+)$/m);
      if (!head || section.indexOf(head[0]) !== 0) continue;
      const body = plain(section.slice(head[0].length).replace(/```[\s\S]*?```/g, ' ').replace(/^\|.*\n\|[\s:|-]+\|\s*$/gm, '').replace(/^\|.*$/gm, m => m.replace(/\|/g, ' ')).replace(/^> \[!\w+\]/gm, ''));
      const id = slugify(head[2]);
      search.push({ t: plain(head[2]), p: meta.title, g: group, u: `${pageUrl(slug)}#${id}`, x: body.slice(0, 220) });
    }
  }
  outputs.set('docs/search.json', JSON.stringify(search) + '\n');
  outputs.set('llms.txt', llms.join('\n') + `\n\n## Optional\n\n- [Guides](${site}/guides/): Beginner walkthroughs with screenshots.\n- [Source code](${repo}): Apache-2.0.\n`);
  outputs.set('llms-full.txt', `# Diri documentation\n${full}\n`);

  // Sitemap: everything that is not docs stays as written; docs follow the nav.
  const sitemap = await readFile(resolve(root, 'sitemap.xml'), 'utf8');
  const kept = sitemap.split('\n').filter(line => line.includes('<url>') && !line.includes('/docs/'));
  const docsUrls = order.map(({ slug }) => `  <url><loc>${site}${pageUrl(slug)}</loc><lastmod>${today}</lastmod></url>`);
  outputs.set('sitemap.xml', `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${[...kept, ...docsUrls].join('\n')}\n</urlset>\n`);
  return { outputs, order };
}

export const docPages = async () => (await buildDocs()).order.map(({ slug }) => slug === 'index' ? 'docs/index.html' : `docs/${slug}/index.html`);

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { outputs } = await buildDocs();
  if (process.argv.includes('--check')) {
    const stale = [];
    for (const [file, content] of outputs) if (await readFile(resolve(root, file), 'utf8').catch(() => null) !== content) stale.push(file);
    if (stale.length) { console.error(`Docs are stale, run \`npm run docs\`:\n  ${stale.join('\n  ')}`); process.exit(1); }
    console.log(`Docs up to date (${outputs.size} files).`);
  } else {
    await rm(resolve(root, 'docs'), { recursive: true, force: true });
    for (const [file, content] of outputs) {
      await mkdir(dirname(resolve(root, file)), { recursive: true });
      await writeFile(resolve(root, file), content);
    }
    console.log(`Wrote ${outputs.size} docs files.`);
  }
}
