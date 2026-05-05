// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type { GenerateResponse, ImportResponse } from '../api/types';

class ProjectState {
  imported = $state<ImportResponse | null>(null);
  generated = $state<GenerateResponse | null>(null);
  loading = $state(false);
  generating = $state(false);
  error = $state<string | null>(null);
  visibleLayers = $state<Set<string>>(new Set());

  setImported(r: ImportResponse) {
    this.imported = r;
    this.generated = null;
    this.error = null;
    this.visibleLayers = new Set(r.layers.map((l) => l.name));
  }

  setGenerated(r: GenerateResponse) {
    this.generated = r;
    this.error = null;
  }

  setError(msg: string) {
    this.error = msg;
  }

  toggleLayer(name: string) {
    const next = new Set(this.visibleLayers);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    this.visibleLayers = next;
  }
}

export const project = new ProjectState();
