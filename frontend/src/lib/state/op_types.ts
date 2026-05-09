// Pure-TypeScript op-type aliases. Lives outside `project.svelte.ts` so
// modules without a Svelte runtime (helpers, vitest specs) can import the
// shapes without dragging in `$state`.

export type ToolKind = 'endmill' | 'ball_nose' | 'v_bit' | 'engraver' | 'drag_knife' | 'drill' | 'laser_beam';

export type OpKind =
  | 'profile'
  | 'pocket'
  | 'drill'
  | 'thread'
  | 'chamfer'
  | 'engrave'
  | 'drag_knife'
  | 'helix'
  | 'vcarve';

export type ProfileOffset = 'outside' | 'inside' | 'on';
export type SourceCombine =
  | 'auto'
  | 'union'
  | 'difference'
  | 'intersection'
  | 'xor'
  | 'none';
export type FrameShape = 'rectangle' | 'rounded_rectangle';
