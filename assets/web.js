// Theme, the / shortcut, and saying so when a search can't reach the server.
// Loaded in <head>, soo the theme is set before the first paint.
const storage = {
  get(key) { try { return localStorage.getItem(key); } catch { return null; } },
  set(key, value) { try { localStorage.setItem(key, value); } catch { /* the choice lasts this page only */ } },
};

const saved = storage.get('theme');
if (saved === 'light' || saved === 'dark') document.documentElement.dataset.theme = saved;

document.addEventListener('click', (event) => {
  if (!event.target.closest('[data-theme-toggle]')) return;
  const root = document.documentElement;
  const current = root.dataset.theme || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
  root.dataset.theme = current === 'dark' ? 'light' : 'dark';
  storage.set('theme', root.dataset.theme);
});

document.addEventListener('keydown', (event) => {
  const box = document.getElementById('q');
  if (event.key !== '/' || !box || event.metaKey || event.ctrlKey || event.altKey) return;
  if (event.target.closest('input, select, textarea')) return;
  event.preventDefault();
  box.focus();
  box.select();
});

// Categories fold into a menu on phones.
const narrow = matchMedia('(max-width: 820px)');
function foldNavigation() {
  const navigation = document.querySelector('.navigation');
  if (navigation) navigation.open = !narrow.matches;
}
document.addEventListener('DOMContentLoaded', foldNavigation);
narrow.addEventListener('change', foldNavigation);

function showFailure(message) {
  const failure = document.getElementById('failure');
  if (!failure) return;
  failure.textContent = '';
  if (!message) return;
  const box = document.createElement('div');
  box.className = 'diag diag-caution';
  box.setAttribute('role', 'alert');
  const head = document.createElement('p');
  head.className = 'diag-head';
  const level = document.createElement('span');
  level.className = 'diag-level';
  level.textContent = 'caution:';
  head.append(level, ' the results did not update');
  const body = document.createElement('div');
  body.className = 'diag-body';
  const text = document.createElement('p');
  text.textContent = message;
  body.append(text);
  box.append(head, body);
  failure.append(box);
}

document.addEventListener('htmx:beforeRequest', () => showFailure(''));
document.addEventListener('htmx:sendError', () => showFailure('Atlas did not answer. Check that it is running, then search again.'));
document.addEventListener('htmx:responseError', (event) => {
  // a 401 redirects to the sign in page by itself
  if (event.detail.xhr.status !== 401) showFailure(`Atlas answered with an error (${event.detail.xhr.status}). Search again.`);
});
document.addEventListener('htmx:afterSwap', () => {
  if (narrow.matches) foldNavigation();
});

// Ask AI: the search box shows the words Gemini searched with. Its value is set, not the box swapped, soo the
// search form keeps listening to it.
document.addEventListener('htmx:afterSettle', () => {
  const read = document.querySelector('#ask-note [data-q]');
  const box = document.getElementById('q');
  if (!read || !box) return;
  box.value = read.dataset.q;
  read.removeAttribute('data-q');
});

// A drawer: the first click loads it (htmx, once), later clicks fold and unfold it. Under a result row the drawer
// is the next row; on a copy card or a copy link it is the card appended inside the copy's body or list item.
document.addEventListener('click', (event) => {
  const toggle = event.target.closest('.drawer-toggle');
  if (!toggle) return;
  const row = toggle.closest('tr');
  const drawer = row
    ? row.nextElementSibling
    : toggle.closest('.copy-body, li')?.querySelector(':scope > .drawer-card');
  if (!drawer || !(drawer.classList.contains('drawer-row') || drawer.classList.contains('drawer-card'))) return;
  drawer.hidden = !drawer.hidden;
  toggle.setAttribute('aria-expanded', String(!drawer.hidden));
});
document.addEventListener('htmx:afterSwap', (event) => {
  const toggle = event.detail.elt;
  if (toggle?.classList?.contains('drawer-toggle')) toggle.setAttribute('aria-expanded', 'true');
});
