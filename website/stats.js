// Live GitHub stars and commit cadence. Nothing renders until the numbers arrive; any failure leaves the page as it was.
const repo = 'https://api.github.com/repos/cristicretu/diri';
const day = 864e5;

const cached = key => {
  try {
    const hit = JSON.parse(sessionStorage.getItem(key) || 'null');
    return hit && Date.now() - hit.at < 36e5 ? hit.value : null;
  } catch { return null; }
};
const remember = (key, value) => { try { sessionStorage.setItem(key, JSON.stringify({ at: Date.now(), value })); } catch {} };

async function getJSON(url) {
  const response = await fetch(url, { headers: { Accept: 'application/vnd.github+json' }, credentials: 'omit' });
  if (!response.ok) throw new Error(String(response.status));
  return response;
}

export async function starCount() {
  const hit = cached('diri.stars');
  if (hit !== null) return hit;
  const stars = (await (await getJSON(repo)).json()).stargazers_count;
  if (!Number.isSafeInteger(stars)) throw new Error('stars');
  remember('diri.stars', stars);
  return stars;
}

// One request: ask for one commit per page and read the page count from the Link header.
export async function commitsSince(days, now = Date.now()) {
  const hit = cached('diri.commits30');
  if (hit !== null) return hit;
  const since = new Date(now - days * day).toISOString();
  const response = await getJSON(`${repo}/commits?per_page=1&since=${encodeURIComponent(since)}`);
  const last = /[?&]page=(\d+)>; rel="last"/.exec(response.headers.get('Link') || '');
  const count = last ? Number(last[1]) : (await response.json()).length;
  remember('diri.commits30', count);
  return count;
}

export const compact = n => n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, '')}k` : String(n);

if (typeof document !== 'undefined') {
  starCount().then(stars => {
    for (const el of document.querySelectorAll('.star-count')) { el.textContent = compact(stars); el.classList.add('loaded'); }
    for (const link of document.querySelectorAll('.github-link')) link.setAttribute('aria-label', `Diri on GitHub, ${stars} stars`);
  }).catch(() => {});
  const cadence = document.querySelector('#cadence');
  // The pill always holds its space; it only fades in, so the headline never moves.
  if (cadence) {
    const label = cadence.querySelector('span:last-child');
    commitsSince(30)
      .then(count => { label.textContent = count >= 20 ? `${count} commits in the last 30 days` : 'See what shipped this week'; })
      .catch(() => { label.textContent = 'See what shipped this week'; })
      .finally(() => cadence.classList.add('loaded'));
  }
}
