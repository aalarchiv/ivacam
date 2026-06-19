// CDP smoke test for the tab freeze: boot the built app, click the
// Machine and Tool library tabs, and verify the JS thread stays
// responsive (the bug was an infinite self-invalidating $effect loop
// that hard-froze the app the moment the tab's panel mounted).
const CDP_PORT = process.env.CDP_PORT ?? '9224';
const APP_URL = process.env.APP_URL ?? 'http://localhost:4173/';

function evalIn(ws, id, expression, awaitPromise = false) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`FROZEN: evaluate #${id} timed out`)), 5000);
    const onMsg = (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id === id) {
        clearTimeout(timer);
        ws.removeEventListener('message', onMsg);
        resolve(msg.result?.result?.value);
      }
    };
    ws.addEventListener('message', onMsg);
    ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, returnByValue: true, awaitPromise } }));
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const res = await fetch(`http://127.0.0.1:${CDP_PORT}/json/new?${encodeURIComponent(APP_URL)}`, { method: 'PUT' });
const target = await res.json();
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.addEventListener('open', r); ws.addEventListener('error', j); });

const exceptions = [];
ws.addEventListener('message', (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.method === 'Runtime.exceptionThrown') {
    exceptions.push(msg.params.exceptionDetails?.exception?.description ?? msg.params.exceptionDetails?.text ?? 'unknown');
  }
});
ws.send(JSON.stringify({ id: 1, method: 'Runtime.enable', params: {} }));

let id = 10;
// Wait for boot (tab strip present).
let booted = false;
for (let i = 0; i < 30; i++) {
  await sleep(500);
  try {
    booted = await evalIn(ws, id++, `document.querySelectorAll('.main-tab').length === 6`);
    if (booted) break;
  } catch { /* still loading */ }
}
console.log('boot:', booted ? 'OK' : 'FAILED');
if (!booted) process.exit(1);

for (const [idx, name] of [[2, 'tools'], [1, 'machine'], [3, 'settings'], [4, 'help'], [5, 'about'], [0, 'project']]) {
  await evalIn(ws, id++, `document.querySelectorAll('.main-tab')[${idx}].click(); true`);
  await sleep(1500);
  // Responsiveness probe: a frozen effect loop never answers this.
  const alive = await evalIn(ws, id++, `1 + 1`).then((v) => v === 2).catch(() => false);
  const visible = await evalIn(
    ws,
    id++,
    idx === 0
      ? `getComputedStyle(document.querySelector('main.split')).display !== 'none'`
      : `[...document.querySelectorAll('main.tab-panel')].some((p) => getComputedStyle(p).display !== 'none')`,
  ).catch(() => false);
  console.log(`tab ${name}: responsive=${alive ? 'OK' : 'FROZEN'} visible=${visible ? 'OK' : 'NO'}`);
  if (!alive) process.exit(2);
}

// Machine sub-tabs: Settings form mounts the embedded MachineDialog.
await evalIn(ws, id++, `document.querySelectorAll('.main-tab')[1].click(); true`);
await sleep(300);
await evalIn(ws, id++, `[...document.querySelectorAll('.sub-tab')].find((b) => b.textContent.trim() === 'Settings')?.click(); true`);
await sleep(1200);
const aliveSettings = await evalIn(ws, id++, `2 + 2`).then((v) => v === 4).catch(() => false);
const settingsVisible = await evalIn(ws, id++, `!!document.querySelector('.embedded-shell .grid')`).catch(() => false);
console.log(`machine>settings: responsive=${aliveSettings ? 'OK' : 'FROZEN'} form=${settingsVisible ? 'OK' : 'NO'}`);

// Tooling chooser rendered?
await evalIn(ws, id++, `[...document.querySelectorAll('.sub-tab')].find((b) => b.textContent.trim() === 'Tooling')?.click(); true`);
await sleep(500);
const tooling = await evalIn(ws, id++, `!!document.querySelector('.tooling')`).catch(() => false);
console.log(`machine>tooling: rendered=${tooling ? 'OK' : 'NO'}`);

console.log('page exceptions:', exceptions.length === 0 ? 'none' : exceptions.slice(0, 3).join(' | '));
process.exit(aliveSettings && tooling ? 0 : 3);
