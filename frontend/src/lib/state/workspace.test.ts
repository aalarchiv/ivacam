/// Workspace persistence tests. Cover the save / load round-trip,
/// recent-project dedup + cap, per-project isolation, schema-version
/// rejection, and clear-recents. Uses an in-memory transport so the
/// tests don't touch localStorage or Tauri.

import { describe, expect, it } from 'vitest';
import {
  DEFAULT_WORKSPACE,
  WorkspaceStore,
  type WorkspaceTransport,
} from './workspace';

class MemoryTransport implements WorkspaceTransport {
  blob: string | null = null;
  async read(): Promise<string | null> {
    return this.blob;
  }
  async write(json: string): Promise<void> {
    this.blob = json;
  }
}

async function freshLoaded(transport: MemoryTransport): Promise<WorkspaceStore> {
  const s = new WorkspaceStore(transport);
  await s.load();
  return s;
}

describe('WorkspaceStore', () => {
  it('load_save_load_round_trip', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    s.update({ camera: { px: 1, py: 2, pz: 3, tx: 4, ty: 5, tz: 6 } });
    s.update({ panels: { left_width: 100, right_width: 200, bottom_height: 300 } });
    s.addRecentProject('/tmp/a.vc-project.json', 'a.vc-project.json');
    s.setPerProject('/tmp/a.vc-project.json', { visible_layers: ['L1', 'L2'], selected_op_id: 7, playhead: 0.5 });
    await s.save();

    const s2 = await freshLoaded(t);
    const w = s2.get();
    expect(w.camera).toEqual({ px: 1, py: 2, pz: 3, tx: 4, ty: 5, tz: 6 });
    expect(w.panels).toEqual({ left_width: 100, right_width: 200, bottom_height: 300 });
    expect(w.recent_projects).toHaveLength(1);
    expect(w.recent_projects[0].path).toBe('/tmp/a.vc-project.json');
    expect(w.last_project).toBe('/tmp/a.vc-project.json');
    expect(w.per_project['/tmp/a.vc-project.json']).toEqual({
      visible_layers: ['L1', 'L2'],
      selected_op_id: 7,
      playhead: 0.5,
    });
  });

  it('add_recent_dedupes_and_reorders', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    s.addRecentProject('/A', 'A');
    s.addRecentProject('/B', 'B');
    s.addRecentProject('/A', 'A');
    const r = s.get().recent_projects;
    expect(r.map((e) => e.path)).toEqual(['/A', '/B']);
    expect(s.get().last_project).toBe('/A');
  });

  it('recent_capped_at_10', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    for (let i = 0; i < 12; i++) {
      s.addRecentProject(`/p${i}`, `p${i}`);
    }
    const r = s.get().recent_projects;
    expect(r).toHaveLength(10);
    // Most-recent first; the two oldest (`/p0`, `/p1`) should be gone.
    expect(r[0].path).toBe('/p11');
    expect(r[r.length - 1].path).toBe('/p2');
    expect(r.find((e) => e.path === '/p0')).toBeUndefined();
    expect(r.find((e) => e.path === '/p1')).toBeUndefined();
  });

  it('per_project_isolated', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    s.setPerProject('/a', { visible_layers: ['x'], selected_op_id: 1, playhead: 0.25 });
    s.setPerProject('/b', { visible_layers: ['y', 'z'], selected_op_id: 2, playhead: 0.75 });
    const pp = s.get().per_project;
    expect(pp['/a']).toEqual({ visible_layers: ['x'], selected_op_id: 1, playhead: 0.25 });
    expect(pp['/b']).toEqual({ visible_layers: ['y', 'z'], selected_op_id: 2, playhead: 0.75 });
    // Mutating one path's entry does not bleed into the other.
    s.setPerProject('/a', { selected_op_id: 99 });
    expect(s.get().per_project['/a'].selected_op_id).toBe(99);
    expect(s.get().per_project['/b'].selected_op_id).toBe(2);
  });

  it('schema_version_mismatch_uses_defaults', async () => {
    const t = new MemoryTransport();
    t.blob = JSON.stringify({
      workspace_schema_version: 9999,
      last_project: '/should/be/ignored',
      recent_projects: [{ path: '/x', filename: 'x', openedAt: 0 }],
      camera: { px: 1, py: 2, pz: 3, tx: 4, ty: 5, tz: 6 },
      panels: { left_width: 1, right_width: 2, bottom_height: 3 },
      per_project: {},
    });
    const s = await freshLoaded(t);
    const w = s.get();
    expect(w.last_project).toBe(DEFAULT_WORKSPACE.last_project);
    expect(w.recent_projects).toEqual([]);
    expect(w.camera).toBeNull();
    expect(w.panels).toEqual(DEFAULT_WORKSPACE.panels);
  });

  it('clear_recent_empties', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    s.addRecentProject('/A', 'A');
    s.addRecentProject('/B', 'B');
    expect(s.get().recent_projects).toHaveLength(2);
    s.clearRecentProjects();
    expect(s.get().recent_projects).toEqual([]);
    expect(s.get().last_project).toBeNull();
  });
});
