/// Workspace persistence tests. Cover the save / load round-trip,
/// recent-project dedup + cap, per-project isolation, schema-version
/// rejection, and clear-recents. Uses an in-memory transport so the
/// tests don't touch localStorage or Tauri.

import { describe, expect, it } from 'vitest';
import { DEFAULT_WORKSPACE, WorkspaceStore, type WorkspaceTransport } from './workspace';

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
    s.update({
      panels: { left_width: 100, right_width: 200, bottom_height: 300, ops_fold_snap: 0.75 },
    });
    s.addRecentProject('/tmp/a.vc-project.json', 'a.vc-project.json');
    s.setPerProject('/tmp/a.vc-project.json', {
      visible_layers: ['L1', 'L2'],
      selected_op_id: 7,
      playhead: 0.5,
    });
    await s.save();

    const s2 = await freshLoaded(t);
    const w = s2.get();
    expect(w.camera).toEqual({ px: 1, py: 2, pz: 3, tx: 4, ty: 5, tz: 6 });
    expect(w.panels).toEqual({
      left_width: 100,
      right_width: 200,
      bottom_height: 300,
      ops_fold_snap: 0.75,
    });
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

  it('last_post_processor_round_trip', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    expect(s.get().last_post_processor).toBe('linuxcnc');
    s.setLastPostProcessor('grbl');
    expect(s.get().last_post_processor).toBe('grbl');
    await s.save();

    const s2 = await freshLoaded(t);
    expect(s2.get().last_post_processor).toBe('grbl');

    // Setting to the current value is a no-op (no spurious save / notify).
    let bumps = 0;
    s2.subscribe(() => {
      bumps += 1;
    });
    s2.setLastPostProcessor('grbl');
    expect(bumps).toBe(0);
    s2.setLastPostProcessor('hpgl');
    expect(bumps).toBe(1);
    expect(s2.get().last_post_processor).toBe('hpgl');
  });

  it('set_panels_merges_and_skips_no_op', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    let bumps = 0;
    s.subscribe(() => {
      bumps += 1;
    });
    s.setPanels({ right_width: 480 });
    expect(s.get().panels).toEqual({
      ...DEFAULT_WORKSPACE.panels,
      right_width: 480,
    });
    expect(bumps).toBe(1);

    // Identical patch is a no-op (no spurious notify / save).
    s.setPanels({ right_width: 480 });
    expect(bumps).toBe(1);

    s.setPanels({ bottom_height: 320 });
    expect(s.get().panels.right_width).toBe(480);
    expect(s.get().panels.bottom_height).toBe(320);
    expect(bumps).toBe(2);
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

describe('machine profiles', () => {
  const sampleProfile = (id: string, name = 'Shop CNC') => ({
    id,
    name,
    machine: { unit: 'mm', mode: 'mill', comments: true, arcs: true } as never,
    tools: [{ id: 1, name: 'em', kind: 'endmill' }] as never[],
  });

  it('upsert / mirror / delete round-trip through save + load', async () => {
    const t = new MemoryTransport();
    const a = await freshLoaded(t);
    a.upsertMachineProfile(sampleProfile('mp-1') as never);
    a.upsertMachineProfile(sampleProfile('mp-2', 'Plasma table') as never);
    await a.save();

    const b = await freshLoaded(t);
    expect(b.get().machine_profiles.map((p) => p.id)).toEqual(['mp-1', 'mp-2']);

    // Mirror updates in place (and follows machine.name when set).
    const edited = sampleProfile('mp-1');
    (edited.machine as Record<string, unknown>).name = 'Renamed CNC';
    b.mirrorMachineProfile('mp-1', edited.machine as never, edited.tools as never);
    expect(b.get().machine_profiles[0].name).toBe('Renamed CNC');
    // Mirror into a missing id is a no-op, not an insert.
    b.mirrorMachineProfile('mp-ghost', edited.machine as never, edited.tools as never);
    expect(b.get().machine_profiles).toHaveLength(2);

    b.deleteMachineProfile('mp-2');
    await b.save();
    const c = await freshLoaded(t);
    expect(c.get().machine_profiles.map((p) => p.id)).toEqual(['mp-1']);
  });

  it('upsert with an existing id replaces in place (stable ordering)', async () => {
    const t = new MemoryTransport();
    const s = await freshLoaded(t);
    s.upsertMachineProfile(sampleProfile('mp-1', 'A') as never);
    s.upsertMachineProfile(sampleProfile('mp-2', 'B') as never);
    s.upsertMachineProfile(sampleProfile('mp-1', 'A2') as never);
    expect(s.get().machine_profiles.map((p) => p.name)).toEqual(['A2', 'B']);
  });

  it('parse drops malformed and duplicate-id profile entries', async () => {
    const t = new MemoryTransport();
    t.blob = JSON.stringify({
      ...DEFAULT_WORKSPACE,
      machine_profiles: [
        sampleProfile('mp-ok'),
        sampleProfile('mp-ok', 'dup id'),
        { id: '', name: 'no id', machine: {}, tools: [] },
        { id: 'mp-no-machine', name: 'x', tools: [] },
        { id: 'mp-no-tools', name: 'x', machine: {} },
        'garbage',
        null,
      ],
    });
    const s = await freshLoaded(t);
    expect(s.get().machine_profiles.map((p) => p.id)).toEqual(['mp-ok']);
  });
});

describe('tool inventory', () => {
  it('round-trips through save + load and drops malformed entries', async () => {
    const t = new MemoryTransport();
    const a = await freshLoaded(t);
    a.setToolInventory([
      { id: 1, name: 'em', kind: 'endmill' } as never,
      { id: 2, name: 'torch', kind: 'plasma_torch' } as never,
    ]);
    await a.save();
    const b = await freshLoaded(t);
    expect(b.get().tool_inventory.map((x) => x.id)).toEqual([1, 2]);

    t.blob = JSON.stringify({
      ...DEFAULT_WORKSPACE,
      tool_inventory: [
        { id: 1, name: 'ok', kind: 'endmill' },
        { id: 1, name: 'dup id', kind: 'endmill' },
        { id: 'nope', name: 'bad id', kind: 'endmill' },
        { id: 3, kind: 'endmill' },
        null,
        'garbage',
      ],
    });
    const c = await freshLoaded(t);
    expect(c.get().tool_inventory.map((x) => x.id)).toEqual([1]);
  });
});
