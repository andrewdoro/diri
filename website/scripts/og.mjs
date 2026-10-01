// Draws the 1200×630 share card for every generated page (og/cards.json) with headless
// Chrome, then saves a JPEG with macOS `sips`. Only cards whose text or design changed
// are redrawn; og/rendered.json records what each file was drawn from.
// Usage: npm run og [-- --all]
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';
import { root, esc } from './render.mjs';
import { cardHash } from './site.mjs';

const run = promisify(execFile);
const chrome = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const svg = async name => `data:image/svg+xml;base64,${(await readFile(resolve(root, `assets/brand/${name}.svg`))).toString('base64')}`;

async function cardHtml(card) {
  const icon = await svg('diri-icon');
  const logo = card.logo ? await svg(card.logo) : null;
  const partner = logo || card.letter
    ? `<span class="plus">+</span><span class="tile">${logo ? `<span class="mark" style="-webkit-mask-image:url('${logo}')"></span>` : `<b>${esc(card.letter)}</b>`}</span>`
    : '';
  const size = card.title.length > 44 ? 64 : card.title.length > 28 ? 76 : 88;
  return `<!doctype html><meta charset="utf-8"><style>
  html,body{margin:0;width:1200px;height:630px;overflow:hidden}
  body{font-family:-apple-system,BlinkMacSystemFont,"SF Pro Display",sans-serif;color:#e0def4;-webkit-font-smoothing:antialiased;
    background:radial-gradient(900px 420px at 12% 112%,#c98f9c99,transparent 70%),radial-gradient(760px 420px at 96% 104%,#8e83c9a6,transparent 70%),radial-gradient(1100px 520px at 50% 128%,#5b4a7acc,transparent 75%),linear-gradient(#191724 0%,#1d1a2b 46%,#2a2340 100%)}
  .wrap{position:absolute;inset:64px 80px 58px}
  .brand{display:flex;align-items:center;gap:16px;font-size:34px;font-weight:650;letter-spacing:-1px}
  .brand img{width:58px;height:58px;border-radius:15px;box-shadow:0 0 0 1px #ffffff1c,0 10px 30px -8px #f2385c66}
  .plus{color:#908caa;font-weight:400;margin:0 2px}
  .tile{display:grid;place-items:center;width:58px;height:58px;border-radius:15px;background:#ffffff12;box-shadow:inset 0 0 0 1px #ffffff1f}
  .mark{width:32px;height:32px;background:#e0def4;-webkit-mask-size:contain;-webkit-mask-repeat:no-repeat;-webkit-mask-position:center}
  .tile b{font-size:30px;font-weight:600}
  .eyebrow{margin-top:70px;font-size:26px;font-weight:500;color:#f0c5c3;letter-spacing:.2px}
  h1{margin:14px 0 0;font-size:${size}px;line-height:1.04;font-weight:560;letter-spacing:-.045em;max-width:1000px;text-wrap:balance}
  .sub{margin-top:20px;font-size:32px;line-height:1.3;color:#cfc8de;max-width:960px;text-wrap:balance}
  .foot{position:absolute;left:0;right:0;bottom:0;display:flex;justify-content:space-between;font-size:24px;color:#b6afc9}
  .foot b{color:#e0def4;font-weight:600}
  </style><div class="wrap"><div class="brand"><img src="${icon}">diri${partner}</div>
  <div class="eyebrow">${esc(card.eyebrow)}</div><h1>${esc(card.title)}</h1>${card.subtitle ? `<div class="sub">${esc(card.subtitle)}</div>` : ''}
  <div class="foot"><span><b>diri.sh</b>${esc(card.url === '/' ? '' : card.url.replace(/\/$/, ''))}</span><span>Free and open source</span></div></div>`;
}

async function draw(card, dir) {
  const html = join(dir, `${card.key}.html`);
  const png = join(dir, `${card.key}.png`);
  await writeFile(html, await cardHtml(card));
  // Chrome sometimes exits non-zero during teardown after the screenshot is written.
  await run(chrome, ['--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1', '--window-size=1200,630', `--screenshot=${png}`, `file://${html}`]).catch(async error => { if (!await stat(png).then(() => true, () => false)) throw error; });
  await run('sips', ['-s', 'format', 'jpeg', '-s', 'formatOptions', '82', png, '--out', resolve(root, `og/${card.key}.jpg`)]);
}

const cards = JSON.parse(await readFile(resolve(root, 'og/cards.json'), 'utf8'));
const renderedPath = resolve(root, 'og/rendered.json');
const rendered = JSON.parse(await readFile(renderedPath, 'utf8').catch(() => '{}'));
const exists = async key => stat(resolve(root, `og/${key}.jpg`)).then(() => true, () => false);
const todo = [];
for (const { hash, ...card } of cards) {
  void hash;
  if (process.argv.includes('--all') || rendered[card.key] !== cardHash(card) || !await exists(card.key)) todo.push(card);
}
const dir = await mkdtemp(join(tmpdir(), 'diri-og-'));
try {
  for (let i = 0; i < todo.length; i += 6) {
    await Promise.all(todo.slice(i, i + 6).map(card => draw(card, dir)));
    for (const card of todo.slice(i, i + 6)) rendered[card.key] = cardHash(card);
    process.stdout.write(`\r${Math.min(i + 6, todo.length)}/${todo.length} cards`);
  }
} finally {
  await rm(dir, { recursive: true, force: true });
}
// Forget cards whose pages no longer exist.
const live = new Set(cards.map(card => card.key));
for (const key of Object.keys(rendered)) if (!live.has(key)) { delete rendered[key]; await rm(resolve(root, `og/${key}.jpg`), { force: true }); }
await writeFile(renderedPath, JSON.stringify(Object.fromEntries(Object.entries(rendered).sort()), null, 2) + '\n');
console.log(`\nDrew ${todo.length} of ${cards.length} share cards.`);
