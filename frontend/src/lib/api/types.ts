// Wrapper around `generated.ts` (the auto-generated openapi-typescript
// output) that exposes friendlier names. The generated file is the source
// of truth — never edit by hand. Regenerate with `pnpm run codegen`.

import type { components } from './generated';

export type Point2 = components['schemas']['Point2'];
export type BBox = components['schemas']['BBox'];
export type Segment = components['schemas']['Segment'];
export type SegmentType = NonNullable<Segment['type']>;
export type Layer = components['schemas']['Layer'];
export type ImportResponse = components['schemas']['ImportResponse'];
export type VersionResponse = components['schemas']['VersionResponse'];
export type HealthResponse = components['schemas']['HealthResponse'];
export type GenerateRequest = components['schemas']['GenerateRequest'];
export type GenerateResponse = components['schemas']['GenerateResponse'];
export type ToolpathSegment = components['schemas']['ToolpathSegment'];
export type ToolpathKind = NonNullable<ToolpathSegment['kind']>;
export type Pose3 = components['schemas']['Pose3'];
export type GenerateStats = components['schemas']['GenerateStats'];
export type RenderTextRequest = components['schemas']['RenderTextRequest'];
export type RenderTextResponse = components['schemas']['RenderTextResponse'];
export type ImportedObject = components['schemas']['ImportedObject'];
