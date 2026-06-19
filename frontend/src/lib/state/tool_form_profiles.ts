import type { FormProfileSample } from './project.svelte';

const round3 = (v: number) => Math.round(v * 1000) / 1000;

export interface DovetailParams {
  diaMm: number;
  angleDeg: number;
  heightMm: number;
}

/// Dovetail / form cutter as a 2-sample form profile: full radius at the
/// tip (z = 0), tapering inward by the flank angle over the cutter height.
export function dovetailProfile({
  diaMm,
  angleDeg,
  heightMm,
}: DovetailParams): FormProfileSample[] {
  const rBottom = Math.max(diaMm / 2, 0);
  const h = Math.max(heightMm, 0);
  const rTop = Math.max(rBottom - h * Math.tan((angleDeg * Math.PI) / 180), 0);
  return [
    { zMm: 0, rMm: round3(rBottom) },
    { zMm: round3(h), rMm: round3(rTop) },
  ];
}

export interface TslotParams {
  headDiaMm: number;
  headThickMm: number;
  neckDiaMm: number;
  neckLenMm: number;
}

/// T-slot cutter as a form profile: a wide cutting disk (headDia) of
/// height headThick at the tip, then a narrow neck (neckDia) up to the
/// top of the neck. The neck radius is clamped to the head radius.
export function tslotProfile({
  headDiaMm,
  headThickMm,
  neckDiaMm,
  neckLenMm,
}: TslotParams): FormProfileSample[] {
  const rHead = Math.max(headDiaMm / 2, 0);
  const rNeck = Math.max(Math.min(neckDiaMm / 2, rHead), 0);
  const hHead = Math.max(headThickMm, 0);
  return [
    { zMm: 0, rMm: round3(rHead) },
    { zMm: round3(hHead), rMm: round3(rHead) },
    { zMm: round3(hHead), rMm: round3(rNeck) },
    { zMm: round3(hHead + Math.max(neckLenMm, 0)), rMm: round3(rNeck) },
  ];
}
