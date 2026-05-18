# kbx5 — OpParams junk drawer → per-kind variants

Draft plan. Read end-to-end, push back on anything before any code lands.

## Goal

Make the type system carry the per-op-kind invariant. Today `op.params.pocket_islands` compiles and runs whether the op is a Pocket or a Profile — it's only ignored downstream by accident. Target: `pocket_islands` is reachable **only** through a `PocketOp` (or however it ends up named), and the compiler refuses misuse.

This is purely a clarity / type-safety win. **Zero behavior change.** The pipeline still cuts the exact same toolpaths.

## Current state (after 3ltz)

`crates/wiac-core/src/project/params.rs` (430 LOC) holds one struct, `OpParams`, with **32 fields**. They fall into four buckets:

### Genuinely universal (10) — stay on a shared common struct

| field | notes |
|---|---|
| `depth`, `start_depth` | depth schedule |
| `step`, `fast_move_z` | depth schedule |
| `through_depth`, `depth_list` | extended Z schedule |
| `finish_step` | bottom-finish pass |
| `feed_rate_override`, `plunge_rate_override` | per-op feed overrides |
| `plunge` | plunge strategy (Direct / Ramp / Helix) |
| `objectorder` | order multiple-object cuts |

### Closed-contour (Profile + Pocket + Engrave + DragKnife) (8)

| field | notes |
|---|---|
| `tabs`, `tab_mode`, `tab_placements` | tab shape + position |
| `leads` | lead-in / lead-out |
| `cut_direction`, `finish_cut_direction` | climb vs conventional |
| `corner_feed_reduction` | slow at sharp corners |
| `approach_point` | user-picked plunge XY |

### Pocket-only (8)

| field | notes |
|---|---|
| `xy_overlap` | cascade stride |
| `pocket_islands`, `pocket_nocontour`, `pocket_insideout` | strategy flags |
| `finish_xy_allowance_mm` | Schlichtzugabe |
| `frame_shape`, `frame_padding_mm`, `frame_corner_radius_mm` | Pocket-Outside wrapper |

### Profile-only (3)

| field | notes |
|---|---|
| `overcut` | dip into inner corners |
| `reverse` | flip cut direction |
| `helix` | helical entry on closed contour |

### Drill-only (1)

`chamfer_after_width_mm` — Stufenfase post-drill chamfer.

### V-Carve-only (2)

`carve_max_width_mm`, `multi_pass_refine`.

### Already-on-OpKind (5)

`OpKind::Thread { pitch_mm, internal, climb }` and `OpKind::Drill { cycle }` already carry their kind-specific data on the variant. `OpKind::Chamfer { width_mm, finish_pass }` too. These don't need to move.

## Target shape

```rust
pub struct Op {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub tool_id: u32,
    pub finish_tool_id: Option<u32>,
    pub source: OpSource,
    pub kind: OpKind,
    pub params: OpParamsCommon,      // universal only (11 fields)
    pub pattern: Option<PatternConfig>,
}

pub struct OpParamsCommon { /* 10 fields above */ }

pub struct ContourParams { /* 8 fields above */ }

pub struct PocketParams { /* 8 fields above */ }

pub struct ProfileParams {
    pub overcut: bool,
    pub reverse: bool,
    pub helix: bool,
}

pub struct VCarveParams {
    pub carve_max_width_mm: Option<f64>,
    pub multi_pass_refine: bool,
}

pub enum OpKind {
    Profile {
        offset: ToolOffset,
        contour: ContourParams,
        profile: ProfileParams,
    },
    Pocket {
        strategy: PocketStrategy,
        contour: ContourParams,
        pocket: PocketParams,
    },
    Engrave { contour: ContourParams },
    DragKnife { contour: ContourParams },
    Drill {
        cycle: DrillCycle,
        chamfer_after_width_mm: Option<f64>,
    },
    Thread { pitch_mm, internal, climb },     // unchanged
    Chamfer { width_mm, finish_pass },        // unchanged
    VCarve { carve: VCarveParams },
    Helix,                                    // unchanged (no params today)
}
```

## Frontend side: most of this work is already done

Commit `8d019ba` (sue, OpEntry refactor) already made the **frontend** type a discriminated union with per-variant interfaces (`PocketOp`, `ProfileOp`, `VCarveOp`, etc.). The frontend already thinks in per-kind terms.

What `build-project.ts` does at the wire boundary today: takes the typed `OpEntry`, casts to a `FlatOp` view, and writes back into the old flat OpParams shape because the **backend** still wants flat. That cast disappears once the BE accepts structured.

**Implication:** the frontend type design exists; this is the BE catching up + a wire-format migration. Frontend changes are mechanical (delete the FlatOp cast, write the structured wire shape).

## Wire-format migration

This is the hairy part.

Every existing `.wiac-project.json` carries:
```json
{
  "kind": {"type": "pocket", "strategy": "cascade"},
  "params": {
    "depth": -2, "start_depth": 0, "step": -1, "fast_move_z": 5,
    "xy_overlap": 0.5, "pocket_islands": false,
    "tabs": {...}, "leads": {...}, ...
  }
}
```

The new shape will be:
```json
{
  "kind": {
    "type": "pocket",
    "strategy": "cascade",
    "contour": {"tabs": {...}, "leads": {...}, ...},
    "pocket": {"xy_overlap": 0.5, "pocket_islands": false, ...}
  },
  "params": {"depth": -2, "start_depth": 0, "step": -1, "fast_move_z": 5}
}
```

### Strategy: bridge deserializer on `Op`

Write a custom `Deserialize` impl on `Op` (not on `OpKind`) that:

1. Reads the raw `kind` blob + the raw `params` blob.
2. **Detects shape** by probing the `params` blob for legacy keys (`xy_overlap`, `tabs`, `leads`, `overcut`, etc.).
3. **Legacy detected**: copy per-kind fields from `params` into the appropriate `kind` variant fields; keep universal fields on the new `OpParamsCommon`.
4. **New shape detected**: pass through unchanged.

This lives in `project/op.rs` (or a helper file) and is unit-tested with fixture JSON for every variant.

**Saves always write the new shape.** Once a project is opened and re-saved, it's permanently on the new format.

Schema regeneration emits the new shape only; old shape is deserializer-only (no schema entry).

### Why not bump the project file version?

We could: bump `ProjectFile.version` to 2, branch on it in `restore()`. Cleaner in some ways. But the deserializer is simple enough that doing it at the `Op` level is fine — we don't need to change `ProjectFile` plumbing. Open question: do we WANT to bump the version anyway as a hygienic flag? See "open questions" below.

## Pipeline reader migration

`grep -rn "op\.params\." crates/wiac-core/src` → 76 references across 13 files. Triage:

- **~40 are universal** (`depth`, `step`, `fast_move_z`, `plunge`, `feed_rate_override`, `objectorder`, `through_depth`, `depth_list`, `finish_step`) — stay on `op.params` (renamed type, same fields).
- **~30 are kind-specific** — must move to `op.kind` matches:
  - `pipeline/op_drivers/vcarve.rs` → `OpKind::VCarve { carve }` match
  - `pipeline/op_drivers/halfpipe.rs` (pocket) → `OpKind::Pocket { contour, pocket, .. }` match
  - `pipeline/op_drivers/drill.rs` → `OpKind::Drill { chamfer_after_width_mm, .. }` match
  - `pipeline/offset_builder.rs` (most touched) → contour-aware branches
  - `pipeline/tabs.rs` → ContourParams accessor
  - `pipeline/frame.rs` → Pocket's frame_shape fields
  - `pipeline/regions.rs` → pocket_islands / pocket_nocontour
  - `pipeline_cache.rs` → hash inputs (audit the key)

Most are dispatched-from-driver, so the match already exists; this just adds destructuring fields.

## Schema + frontend codegen

- `cargo xtask schema` regenerates `schema/openapi.yaml` against the new types. The diff will be **large** — per-kind structs become schema components.
- `pnpm run codegen` regenerates `frontend/src/lib/api/generated.ts`.
- `build-project.ts` — delete the `FlatOp` cast (lines 28–69) and write structured. Field names follow the codegen.
- Wire-side per-kind sub-objects need names; suggest `contourParams`, `pocketParams`, `vcarveParams` so they don't shadow `pocket`/`vcarve` discriminator labels.

## Suggested sequence

Six commits, each shippable on its own, each passes all gates:

1. **`feat(project): introduce OpParamsCommon, ContourParams, PocketParams, ProfileParams, VCarveParams`** — define the new structs in `project/params.rs`. No callers yet. Embed them in OpKind variants as new fields **alongside** the existing OpParams flat fields. Make Op carry BOTH the flat `params` and the new variant-embedded structs at first. Custom Deserialize copies legacy fields into the new structs on load; Serialize emits both for forward-compat reading. Tests verify round-trip.

2. **`refactor(pipeline): read kind-specific fields through the variant structs`** — migrate all 30 kind-specific readers in pipeline/* + cam/* to read from `op.kind`'s embedded structs instead of `op.params.X`. The flat fields stay on OpParams but become unused. Tests + golden corpus stay green.

3. **`refactor(project): drop kind-specific fields from OpParams`** — delete `xy_overlap`, `pocket_islands`, `tabs`, etc. from OpParams. `OpParams` is now `OpParamsCommon` (rename the type too). Deserializer still accepts legacy shape; serializer now writes new shape only. Test fixtures: legacy JSON loads correctly; new JSON loads correctly; round-trip stable.

4. **`feat(api): regenerate openapi.yaml + generated.ts`** — `cargo xtask schema && pnpm run codegen`. Commit both. Wire format is now structured.

5. **`refactor(frontend): drop FlatOp cast in build-project.ts`** — write the structured shape directly from the already-typed `OpEntry`. The cast disappears; `build-project.ts` shrinks.

6. **`docs: ARCHITECTURE.md note about per-kind variant params`** — one paragraph + a pointer to the recipe.

Steps 1 + 2 are the bulk of the work. 3–6 are mechanical.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Wire-format migration deserializer has a corner case nobody tested | Unit tests per variant with both legacy and new JSON fixtures. Plus golden-corpus smoke that loads real saved projects. |
| Pipeline readers miss a field during migration | Step 2 lands BEFORE step 3 deletes the flat fields. If a reader still touches `op.params.xy_overlap`, the compile fails. |
| The pipeline cache key (`pipeline_cache.rs`) silently changes if fields move | Step 1 PR includes an explicit test: hashing an Op with PocketParams produces the same cache key as the legacy-flat-field Op carrying the same values. Or: deliberately bump the cache version. |
| Frontend codegen changes break consumers (OpPropertiesPanel, etc.) | Each consumer already discriminates on `op.kind`. The codegen change replaces "every field present optionally" with "fields present per variant" — usually mechanical. Estimate ~10 component touches, each one-line. |
| A field's classification is wrong (e.g. `overcut` is actually meaningful on Pocket too) | Audit each field individually in step 1 by grepping its readers. The current code is the ground truth. |

## Decisions

1. **Skip `ProjectFile.version` bump.** No release yet, no users on disk.
2. **Keep `OpKind::Helix`.** Empty today but earmarked for future thread-mill style work.
3. **`pattern` becomes per-kind, enabled only on `OpKind::Drill` for now.** Other kinds lose the field; if patterning other kinds is needed later, add the field to that variant. Legacy ops carrying a pattern on a non-Drill kind drop the pattern at load with a deserializer warning.
4. **Rename `OpParams` → `OpParamsCommon`** for explicit clarity. A junior who sees the new name knows immediately it's universal-only.
5. **Step 1 intermediate state** (both flat + new fields on Op) is OK during transition, must be fully cleaned up by step 3.

## Estimated effort

- Steps 1–2 together: ~4-6 hrs of careful work + tests
- Step 3: 1 hr (mechanical deletions + deserializer test fixtures)
- Steps 4–6: 1-2 hrs

Total: roughly **one focused day**. Doable in one sitting if uninterrupted; safer split across two so the migration deserializer has overnight to soak.
