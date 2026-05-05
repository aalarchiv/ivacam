// Global project state, Svelte 5 runes.
// Holds the most recently imported geometry plus UI flags.

import type { DefaultsResponse, JsonSchema } from '../api/client';
import type { GenerateResponse, ImportResponse } from '../api/types';

class ProjectState {
  imported = $state<ImportResponse | null>(null);
  generated = $state<GenerateResponse | null>(null);
  loading = $state(false);
  generating = $state(false);
  error = $state<string | null>(null);
  visibleLayers = $state<Set<string>>(new Set());

  /// Setup tree (machine/tool/mill/pockets/tabs/leads). Hydrated from
  /// /defaults; user edits replace the in-memory copy.
  setup = $state<Record<string, unknown>>({});
  setupSchema = $state<JsonSchema | null>(null);
  setupDefinitions = $state<Record<string, JsonSchema>>({});

  /// Extra app-level UI state we want round-tripped to .vc-project.
  selectedEntities = $state<Set<number>>(new Set());

  /// Toolpath scrub position in [0, 1]. Read by Scene3D for the tool-tip
  /// indicator and by PlaybackBar for the slider.
  playhead = $state(1.0);

  setImported(r: ImportResponse) {
    this.imported = r;
    this.generated = null;
    this.error = null;
    this.visibleLayers = new Set(r.layers.map((l) => l.name));
    this.selectedEntities = new Set();
  }

  setGenerated(r: GenerateResponse) {
    this.generated = r;
    this.error = null;
    this.playhead = 1.0;
  }

  setDefaults(d: DefaultsResponse) {
    this.setup = d.setup;
    this.setupSchema = d.schema;
    this.setupDefinitions = d.definitions;
  }

  setSetup(next: Record<string, unknown>) {
    this.setup = next;
    // Discard any prior toolpath — the setup change invalidates it.
    this.generated = null;
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

  /// Snapshot for project save.
  snapshot(): ProjectFile {
    return {
      version: 1,
      kind: 'wiac-project',
      imported: this.imported,
      setup: this.setup,
      visibleLayers: [...this.visibleLayers],
      selectedEntities: [...this.selectedEntities],
    };
  }

  restore(file: ProjectFile) {
    if (file.kind !== 'wiac-project') {
      throw new Error('not a wiaConstructor project file');
    }
    if (file.imported) this.setImported(file.imported);
    this.setup = file.setup ?? this.setup;
    this.visibleLayers = new Set(file.visibleLayers ?? []);
    this.selectedEntities = new Set(file.selectedEntities ?? []);
  }
}

export interface ProjectFile {
  kind: 'wiac-project';
  version: 1;
  imported: ImportResponse | null;
  setup: Record<string, unknown>;
  visibleLayers: string[];
  selectedEntities: number[];
}

export const project = new ProjectState();
