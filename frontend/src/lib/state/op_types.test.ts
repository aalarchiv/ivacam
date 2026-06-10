/// `isProgramOnlyOp` is the FE mirror of the Rust
/// `Op::is_program_only()` predicate at
/// `crates/ivac-core/src/project/op.rs`. The two MUST agree —
/// FE-side row-validation skips tool-existence checks for these
/// kinds, and the Rust pipeline routes them through inline emit
/// blocks that bypass the cutting-op machinery. A drift between
/// the two surfaces as the bug this test guards: a Marker / Probe
/// / Homing / GcodeInclude op rendered with a red "✘ Tool #0 not
/// in tool library" row in OperationsList.svelte, even though the
/// pipeline emits the op cleanly.

import { describe, expect, it } from 'vitest';
import { isProgramOnlyOp, type OpKind } from './op_types';

describe('isProgramOnlyOp', () => {
  it('returns true for every program-only kind', () => {
    const programOnly: OpKind[] = ['pause', 'homing', 'probe', 'cycle_marker', 'gcode_include'];
    for (const k of programOnly) {
      expect(isProgramOnlyOp(k)).toBe(true);
    }
  });

  it('returns false for every cutting / engraving kind', () => {
    // Everything else in the OpKind union — these all carry a
    // cutter and a source geometry, and the tool-existence
    // validator MUST flag tool_id: 0 / missing-tool on them.
    const cutting: OpKind[] = [
      'profile',
      'pocket',
      'drill',
      'thread',
      'chamfer',
      'engrave',
      'drag_knife',
      't_slot',
      'dovetail',
      'vcarve',
      'relief_mill',
    ];
    for (const k of cutting) {
      expect(isProgramOnlyOp(k)).toBe(false);
    }
  });
});
