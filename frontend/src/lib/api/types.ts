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
export type RegionPreview = components['schemas']['RegionPreview'];
export type ToolpathSegment = components['schemas']['ToolpathSegment'];
export type ToolpathKind = NonNullable<ToolpathSegment['kind']>;
export type Pose3 = components['schemas']['Pose3'];
export type GenerateStats = components['schemas']['GenerateStats'];
export type TimeEstimate = components['schemas']['TimeEstimate'];
export type RenderTextRequest = components['schemas']['RenderTextRequest'];
export type RenderTextResponse = components['schemas']['RenderTextResponse'];
export type RenderTextLayerResponse = components['schemas']['RenderTextLayerResponse'];
/// Wire-shape TextLayer. `font_bytes` rides as a base64 string —
/// the same form the in-memory TextLayer in state/project.svelte.ts already
/// keeps, so callers pass `bytes_b64` straight through.
export type WireTextLayer = components['schemas']['TextLayer'];
export type HelixRadiusRequest = components['schemas']['HelixRadiusRequest'];
export type HelixRadiusResponse = components['schemas']['HelixRadiusResponse'];
export type ImportedObject = components['schemas']['ImportedObject'];
export type ImportedTextEntity = components['schemas']['ImportedTextEntity'];
export type ImportedTextKind = components['schemas']['ImportedTextKind'];
export type SimWarning = components['schemas']['SimWarning'];
export type SimDiagnostics = components['schemas']['SimDiagnostics'];
export type SimSeverity = components['schemas']['SimSeverity'];

/// Structured backend error: kind + message + optional recovery hint + optional auto-fix.
/// Surfaced through `project.error`; rendered by `ErrorToast.svelte`.
export type WiacError = components['schemas']['WiacError'];
export type WiacErrorKind = components['schemas']['WiacErrorKind'];
export type WiacAutoFix = components['schemas']['WiacAutoFix'];
export type WiacSourceSpan = components['schemas']['WiacSourceSpan'];
