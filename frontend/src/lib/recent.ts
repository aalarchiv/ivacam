// Recent-files list. Persisted via tauri-plugin-store on the desktop and
// localStorage on the web. Capped at MAX entries; most-recent first.

const MAX = 10;
const STORAGE_KEY = 'wiac.recent';

export interface RecentEntry {
  path: string;
  filename: string;
  /** ISO timestamp of last open. */
  lastOpened: string;
}

async function tauriStore() {
  const { Store } = await import('@tauri-apps/plugin-store');
  return await Store.load('settings.json');
}

function isTauri(): boolean {
  if (typeof window === 'undefined') return false;
  return typeof (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ !== 'undefined';
}

export async function readRecent(): Promise<RecentEntry[]> {
  if (isTauri()) {
    try {
      const store = await tauriStore();
      const v = await store.get<RecentEntry[]>('recent');
      return Array.isArray(v) ? v : [];
    } catch {
      return [];
    }
  }
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const v = JSON.parse(raw);
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

export async function pushRecent(entry: RecentEntry): Promise<RecentEntry[]> {
  const cur = (await readRecent()).filter((e) => e.path !== entry.path);
  cur.unshift(entry);
  const trimmed = cur.slice(0, MAX);
  if (isTauri()) {
    try {
      const store = await tauriStore();
      await store.set('recent', trimmed);
      await store.save();
    } catch {
      // ignore — best-effort persistence
    }
  } else {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    } catch {
      // ignore — quota etc.
    }
  }
  return trimmed;
}

export async function clearRecent(): Promise<void> {
  if (isTauri()) {
    try {
      const store = await tauriStore();
      await store.set('recent', []);
      await store.save();
    } catch {
      // ignore
    }
  } else {
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      // ignore
    }
  }
}
