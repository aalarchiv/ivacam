#!/usr/bin/env python3
"""Regenerate golden gcode references using the upstream Python viaConstructor.

Walks `refs/viaconstructor/tests/data/*.dxf`, runs each through the Python
CAM pipeline under a small setup matrix, and writes the gcode into
`tests/golden/expected/<name>.<setup>.expected.gcode`.

Run:
    bridge/.venv/bin/python tests/golden/refresh.py
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "refs/viaconstructor/tests/data"
OUT = ROOT / "tests/golden/expected"
OUT.mkdir(parents=True, exist_ok=True)

SETUPS = {
    "default": [],
    "inside": ["--mill-offset", "inside"],
    "outside-2mm": ["--tool-diameter", "2"],
}


def regen(dxf: Path) -> None:
    for label, args in SETUPS.items():
        target = OUT / f"{dxf.stem}.{label}.expected.gcode"
        cmd = [
            sys.executable,
            "-m",
            "viaconstructor",
            "--headless",
            "--output",
            str(target),
            str(dxf),
            *args,
        ]
        try:
            subprocess.run(
                cmd,
                check=True,
                cwd=ROOT / "refs/viaconstructor",
                env={
                    **os.environ,
                    "QT_QPA_PLATFORM": "offscreen",
                },
                capture_output=True,
            )
            print(f"  ✓ {target.name}")
        except subprocess.CalledProcessError as exc:
            stderr = exc.stderr.decode("utf-8", errors="replace")[:200]
            print(f"  ✗ {target.name} ({stderr.strip()[:120]})")


def main() -> int:
    if not DATA.is_dir():
        print(f"missing {DATA}", file=sys.stderr)
        return 2
    dxfs = sorted(DATA.glob("*.dxf"))
    print(f"refreshing {len(dxfs)} DXF reference(s) under {OUT}")
    for dxf in dxfs:
        print(dxf.name)
        regen(dxf)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
