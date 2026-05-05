"""FastAPI app exposing the wiaConstructor JSON contract.

Endpoints map 1:1 to schema/openapi.yaml. The Rust server, Tauri commands,
and WASM bindings will mirror this surface.
"""

from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path
from typing import Any

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel, Field

from . import __version__

# Make viaConstructor importable. The repo lives at <root>/refs/viaconstructor.
_ROOT = Path(__file__).resolve().parents[2]
_VIACONSTRUCTOR = _ROOT / "refs" / "viaconstructor"
if str(_VIACONSTRUCTOR) not in sys.path:
    sys.path.insert(0, str(_VIACONSTRUCTOR))

# Lazy-imported below so module load failures surface in /healthz, not boot.

app = FastAPI(
    title="wiaConstructor bridge",
    version=__version__,
    description="Stage-1 FastAPI service wrapping the existing Python core.",
)

# Permissive CORS for local dev. The Rust server will tighten this in d0d.10.
app.add_middleware(
    CORSMiddleware,
    allow_origins=os.environ.get("WIAC_CORS_ORIGINS", "*").split(","),
    allow_credentials=False,
    allow_methods=["GET", "POST"],
    allow_headers=["*"],
)


# ─── schema (mirrors schema/openapi.yaml) ──────────────────────────────────


class Point2(BaseModel):
    x: float
    y: float


class BBox(BaseModel):
    min_x: float
    min_y: float
    max_x: float
    max_y: float


class Segment(BaseModel):
    type: str = Field(description="LINE, ARC, CIRCLE or POINT")
    start: Point2
    end: Point2
    bulge: float = 0.0
    center: Point2 | None = None
    layer: str = "0"
    color: int = 7


class Layer(BaseModel):
    name: str
    color: int
    segment_count: int


class ImportResponse(BaseModel):
    filename: str
    format: str
    segments: list[Segment]
    layers: list[Layer]
    bbox: BBox
    unit_scale: float
    warnings: list[str] = Field(default_factory=list)


class HealthResponse(BaseModel):
    ok: bool


class VersionResponse(BaseModel):
    version: str
    transport: str
    capabilities: list[str]


class GenerateRequest(BaseModel):
    segments: list[Segment]
    setup: dict[str, Any] = Field(default_factory=dict)
    post_processor: str = "gcode_linuxcnc"


class Pose3(BaseModel):
    x: float
    y: float
    z: float


class ToolpathSegment(BaseModel):
    from_: Pose3 = Field(alias="from")
    to: Pose3
    kind: str

    model_config = {"populate_by_name": True}


class GenerateResponse(BaseModel):
    gcode: str
    toolpath: list[ToolpathSegment]
    stats: dict[str, Any] = Field(default_factory=dict)


# ─── endpoints ─────────────────────────────────────────────────────────────


@app.get("/healthz", response_model=HealthResponse)
def healthz() -> HealthResponse:
    return HealthResponse(ok=True)


@app.get("/version", response_model=VersionResponse)
def version() -> VersionResponse:
    return VersionResponse(
        version=__version__,
        transport="python-bridge",
        capabilities=["import-dxf", "import-svg", "import-hpgl"],
    )


_SUFFIX_FORMATS = {
    ".dxf": "dxf",
    ".svg": "svg",
    ".hpgl": "hpgl",
    ".plt": "hpgl",
    ".ngc": "ngc",
    ".stl": "stl",
}


@app.post("/import", response_model=ImportResponse)
async def import_file(
    file: UploadFile = File(...),
    format: str | None = Form(default=None),
) -> ImportResponse:
    """Parse a vector file and return wiaConstructor segments.

    Stage-1 wraps `viaconstructor.input_plugins.dxfread.DrawReader` directly.
    Other formats route through the Python plugins as well.
    """
    suffix = Path(file.filename or "").suffix.lower()
    fmt = format or _SUFFIX_FORMATS.get(suffix)
    if fmt is None:
        raise HTTPException(
            status_code=400,
            detail=f"unknown format for filename {file.filename!r}; pass ?format=…",
        )

    # Save to temp because viaConstructor's plugins read by filename.
    contents = await file.read()
    with tempfile.NamedTemporaryFile(suffix=suffix or f".{fmt}", delete=False) as fh:
        fh.write(contents)
        tmp_path = fh.name

    warnings: list[str] = []
    try:
        try:
            reader = _make_reader(fmt, tmp_path)
        except Exception as exc:  # noqa: BLE001
            raise HTTPException(status_code=422, detail=f"parse error: {exc}") from exc

        out_segments: list[Segment] = []
        layer_counts: dict[str, int] = {}
        layer_colors: dict[str, int] = {}

        for seg in reader.segments:
            sx, sy = seg.start[0], seg.start[1]
            ex, ey = seg.end[0], seg.end[1]
            cx, cy = (seg.center[0], seg.center[1]) if seg.center else (None, None)
            out_segments.append(
                Segment(
                    type=seg.type or "LINE",
                    start=Point2(x=sx, y=sy),
                    end=Point2(x=ex, y=ey),
                    bulge=float(seg.bulge or 0.0),
                    center=(Point2(x=cx, y=cy) if cx is not None else None),
                    layer=seg.layer or "0",
                    color=int(seg.color),
                )
            )
            layer_counts[seg.layer] = layer_counts.get(seg.layer, 0) + 1
            if seg.layer not in layer_colors:
                layer_colors[seg.layer] = int(seg.color)

        bbox = BBox(
            min_x=reader.min_max[0],
            min_y=reader.min_max[1],
            max_x=reader.min_max[2],
            max_y=reader.min_max[3],
        )
        layers = [
            Layer(name=name, color=layer_colors[name], segment_count=count)
            for name, count in sorted(layer_counts.items())
        ]
        unit_scale = float(getattr(reader, "scale", 1.0))

        return ImportResponse(
            filename=file.filename or "",
            format=fmt,
            segments=out_segments,
            layers=layers,
            bbox=bbox,
            unit_scale=unit_scale,
            warnings=warnings,
        )
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


def _make_reader(fmt: str, path: str):
    """Dispatch to the right viaConstructor input plugin.

    Imports are done lazily so a missing optional dep doesn't tank the
    whole service.
    """
    if fmt == "dxf":
        from viaconstructor.input_plugins.dxfread import DrawReader

        return DrawReader(path)
    if fmt == "svg":
        from viaconstructor.input_plugins.svgread import DrawReader

        return DrawReader(path)
    if fmt == "hpgl":
        from viaconstructor.input_plugins.hpglread import DrawReader

        return DrawReader(path)
    if fmt == "ngc":
        from viaconstructor.input_plugins.ngcread import DrawReader

        return DrawReader(path)
    if fmt == "stl":
        from viaconstructor.input_plugins.stlread import DrawReader

        return DrawReader(path)
    raise ValueError(f"unsupported format: {fmt}")


@app.post("/generate", response_model=GenerateResponse)
def generate(_request: GenerateRequest) -> GenerateResponse:
    """Generate gcode + 3D toolpath from imported geometry.

    Stage-1 stub. Wiring through to viaConstructor's calc + machine_cmd
    requires pyclipper / cavalier-contours which currently fail to build
    on Python 3.13 — tracked in d0d follow-ups.
    """
    raise HTTPException(
        status_code=501,
        detail="generate not yet wired in Stage-1 bridge (pyclipper unavailable on 3.13)",
    )


@app.get("/defaults")
def defaults() -> dict[str, Any]:
    """Return the default setup tree + a JSON Schema for the form UI."""
    # Stage-1 stub: return an empty shell so the frontend can wire up the
    # plumbing. Real schema generation lands when the Rust core takes over.
    return {
        "setup": {},
        "schema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "title": "Setup tree (placeholder)",
            "properties": {},
        },
    }


def main() -> None:
    """Entry point used by `python -m wiac_bridge`."""
    import uvicorn

    host = os.environ.get("WIAC_HOST", "127.0.0.1")
    port = int(os.environ.get("WIAC_PORT", "8765"))
    uvicorn.run(
        "wiac_bridge.main:app",
        host=host,
        port=port,
        reload=os.environ.get("WIAC_RELOAD", "0") == "1",
    )


if __name__ == "__main__":
    main()
