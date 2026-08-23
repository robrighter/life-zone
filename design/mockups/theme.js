/* Palette switcher. Persists across pages so you can walk the whole app in one
   palette and judge it in situ rather than from swatches. */

const PALETTES = [
  { id: 'unlit',  name: 'Unlit',  sw: '#F0A93C' },
  { id: 'survey', name: 'Survey', sw: '#005E3E' },
  { id: 'strata', name: 'Strata', sw: '#E8823A' }
];

function currentPalette() {
  // ?palette=survey wins, so a palette can be linked to or screenshotted directly
  const q = new URLSearchParams(location.search).get('palette');
  if (q && PALETTES.some(p => p.id === q)) return q;
  try { return localStorage.getItem('lz-palette') || 'unlit'; } catch (e) { return 'unlit'; }
}

function applyPalette(id) {
  if (id === 'unlit') document.documentElement.removeAttribute('data-palette');
  else document.documentElement.setAttribute('data-palette', id);
  try { localStorage.setItem('lz-palette', id); } catch (e) {}
  document.querySelectorAll('.pal-switch button').forEach(b =>
    b.setAttribute('aria-pressed', String(b.dataset.pal === id)));
  // terrain lives in canvas, not CSS, so the map has to be told
  if (typeof setTerrainPalette === 'function') setTerrainPalette(id);
  document.dispatchEvent(new CustomEvent('palettechange', { detail: id }));
}

/* applied before first paint to avoid a flash of the default palette */
applyPalette(currentPalette());

document.addEventListener('DOMContentLoaded', () => {
  document.querySelectorAll('[data-pal-switch]').forEach(mount => {
    mount.className = 'pal-switch';
    mount.innerHTML = '<span class="eyebrow">Palette</span>' + PALETTES.map(p =>
      `<button data-pal="${p.id}" style="--sw:${p.sw}" aria-pressed="${p.id === currentPalette()}">${p.name}</button>`
    ).join('');
    mount.querySelectorAll('button').forEach(b =>
      b.addEventListener('click', () => applyPalette(b.dataset.pal)));
  });
  applyPalette(currentPalette());
});
