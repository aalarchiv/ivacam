/// External-link handling for the embedded webview. Tauri blocks
/// navigation away from the app, so plain <a href="https://…"> clicks
/// silently do nothing — route them through the opener plugin (system
/// browser) instead; browser builds open a new tab. Used as a click
/// delegate on {@html} markdown content (About) where we can't attach
/// per-anchor handlers.
import { isTauri } from '../api/env';

export async function openExternal(url: string): Promise<void> {
  if (isTauri()) {
    const { openUrl } = await import('@tauri-apps/plugin-opener');
    await openUrl(url);
  } else {
    window.open(url, '_blank', 'noopener,noreferrer');
  }
}

/// Click delegate: intercept anchor clicks bubbling out of rendered
/// markdown and open them externally.
export function onExternalLinkClick(e: MouseEvent): void {
  const a = (e.target as HTMLElement | null)?.closest('a[href]');
  if (!a) return;
  const href = a.getAttribute('href') ?? '';
  if (!/^https?:\/\//i.test(href)) return;
  e.preventDefault();
  void openExternal(href);
}
