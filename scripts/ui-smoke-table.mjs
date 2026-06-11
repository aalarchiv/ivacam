// Runtime smoke for the tool-table view: seed a 120-tool inventory via
// localStorage, then verify filtering, sorting, and pagination in the
// rendered Tool library tab.
const CDP_PORT = process.env.CDP_PORT ?? '9224';
const APP_URL = process.env.APP_URL ?? 'http://localhost:4173/';

function evalIn(ws, id, expression) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`FROZEN: evaluate #${id} timed out`)), 6000);
    const onMsg = (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id === id) {
        clearTimeout(timer);
        ws.removeEventListener('message', onMsg);
        resolve(msg.result?.result?.value);
      }
    };
    ws.addEventListener('message', onMsg);
    ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, returnByValue: true } }));
  });
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const res = await fetch(`http://127.0.0.1:${CDP_PORT}/json/new?${encodeURIComponent(APP_URL)}`, { method: 'PUT' });
const target = await res.json();
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.addEventListener('open', r); ws.addEventListener('error', j); });
let id = 10;

// Wait for first boot (localStorage is per-origin, so the seed has to
// happen ON the app origin), then seed 120 inventory tools and reload.
await sleep(4000);
await evalIn(ws, id++, `
  const tools = Array.from({length: 120}, (_, i) => ({
    id: i + 1,
    name: 'tool-' + String(i + 1).padStart(3, '0'),
    kind: i % 10 === 0 ? 'plasma_torch' : 'endmill',
    diameter: ((i % 12) + 1) * 0.5,
    flutes: 2, speed: 18000, plungeRate: 100, feedRate: 800, coolant: 'off',
  }));
  const ws0 = { workspace_schema_version: 1, last_project: null, recent_projects: [],
    camera: null, panels: { left_width: 0, right_width: 360, bottom_height: 240 },
    per_project: {}, last_post_processor: 'linuxcnc', machine_profiles: [],
    tool_inventory: tools };
  localStorage.setItem('ivac-workspace', JSON.stringify(ws0));
  location.reload();
  true
`);
await sleep(5000);
const booted = await evalIn(ws, id++, `document.querySelectorAll('.main-tab').length === 3`);
console.log('boot:', booted ? 'OK' : 'FAILED');
if (!booted) process.exit(1);

await evalIn(ws, id++, `document.querySelectorAll('.main-tab')[2].click(); true`);
await sleep(1200);

const r1 = await evalIn(ws, id++, `({
  rows: document.querySelectorAll('.table .row:not(.head)').length,
  pager: document.querySelector('.table-pager')?.textContent.replace(/\\s+/g,' ').trim() ?? null,
  count: document.querySelector('.tc-count')?.textContent ?? null,
})`);
console.log('page1:', JSON.stringify(r1));

// Filter to torches: 12 rows, pager disappears.
await evalIn(ws, id++, `
  const inp = document.querySelector('.tc-search');
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  set.call(inp, 'plasma');
  inp.dispatchEvent(new Event('input', { bubbles: true }));
  true`);
await sleep(400);
const r2 = await evalIn(ws, id++, `({
  rows: document.querySelectorAll('.table .row:not(.head)').length,
  pager: !!document.querySelector('.table-pager'),
  firstName: document.querySelector('.table .row:not(.head) input[type=text]')?.value ?? null,
})`);
console.log('filtered:', JSON.stringify(r2));

// Clear, sort by diameter desc (two clicks), check first row.
await evalIn(ws, id++, `document.querySelector('.tc-clear')?.click(); true`);
await sleep(200);
await evalIn(ws, id++, `[...document.querySelectorAll('.sort-h')].find(b => b.textContent.includes('⌀')).click(); true`);
await sleep(200);
await evalIn(ws, id++, `[...document.querySelectorAll('.sort-h')].find(b => b.textContent.includes('⌀')).click(); true`);
await sleep(300);
const r3 = await evalIn(ws, id++, `({
  arrow: [...document.querySelectorAll('.sort-h')].find(b => b.textContent.includes('⌀')).textContent.includes('▼'),
  firstDia: document.querySelectorAll('.table .row:not(.head) input[type=number]')[0]?.value ?? null,
})`);
console.log('sorted-desc:', JSON.stringify(r3));

// Pagination: next page works.
await evalIn(ws, id++, `[...document.querySelectorAll('.table-pager button')].at(-1).click(); true`);
await sleep(300);
const r4 = await evalIn(ws, id++, `document.querySelector('.table-pager span')?.textContent ?? null`);
console.log('pager-next:', JSON.stringify(r4));

const ok = r1.rows === 50 && r1.pager?.includes('1 of 3') && r2.rows === 12 && r2.pager === false
  && r3.arrow === true && r3.firstDia === '6' && r4?.includes('2 of 3');
console.log(ok ? 'TABLE SMOKE: PASS' : 'TABLE SMOKE: FAIL');
process.exit(ok ? 0 : 3);
