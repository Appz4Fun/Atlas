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
