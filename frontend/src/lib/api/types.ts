// Hand-typed mirror of schema/openapi.yaml.
// Replaced by openapi-typescript-generated types in d0d.3.

export interface Point2 {
  x: number;
  y: number;
}

export interface BBox {
  min_x: number;
  min_y: number;
  max_x: number;
  max_y: number;
}

export type SegmentType = 'LINE' | 'ARC' | 'CIRCLE' | 'POINT';

export interface Segment {
  type: SegmentType;
  start: Point2;
  end: Point2;
  bulge: number;
  center?: Point2;
  layer: string;
  color: number;
}

export interface Layer {
  name: string;
  color: number;
  segment_count: number;
}

export interface ImportResponse {
  filename: string;
  format: string;
  segments: Segment[];
  layers: Layer[];
  bbox: BBox;
  unit_scale: number;
  warnings: string[];
}

export interface VersionResponse {
  version: string;
  transport: 'python-bridge' | 'rust-server' | 'tauri' | 'wasm';
  capabilities: string[];
}

export interface Pose3 {
  x: number;
  y: number;
  z: number;
}

export type ToolpathKind = 'rapid' | 'cut' | 'plunge' | 'retract' | 'arc';

export interface ToolpathSegment {
  from: Pose3;
  to: Pose3;
  kind: ToolpathKind;
}

export interface GenerateRequest {
  segments: Segment[];
  setup?: Record<string, unknown>;
  post_processor?: 'linuxcnc' | 'grbl' | 'hpgl';
}

export interface GenerateResponse {
  gcode: string;
  toolpath: ToolpathSegment[];
  stats: { object_count: number; closed_object_count: number; offset_count: number };
}

export interface HealthResponse {
  ok: boolean;
}
