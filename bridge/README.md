# wiac-bridge — Stage-1 Python FastAPI service

Wraps the existing Python viaConstructor importer behind the JSON contract in
`schema/openapi.yaml`. This is throwaway scaffolding so the web frontend can
ship before the Rust port lands. See `bd show wiaconstructor-d0d` for the full
plan.

## Setup

```bash
cd bridge
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

## Run

```bash
.venv/bin/python -m wiac_bridge
# or with autoreload:
WIAC_RELOAD=1 .venv/bin/python -m wiac_bridge
```

Defaults: `127.0.0.1:8765`. Override with `WIAC_HOST` / `WIAC_PORT`.

OpenAPI docs at `http://127.0.0.1:8765/docs`.

## Try it

```bash
curl -F file=@../refs/viaconstructor/tests/data/simple.dxf http://127.0.0.1:8765/import | jq '.segments | length'
```
