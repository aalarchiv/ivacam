# wiac-core pipeline baseline

End-to-end time per fixture: DXF import → segments → objects → parallel
offset → linuxcnc gcode emit. Captured by
`cargo bench -p wiac-core --bench pipeline`.

> Numbers below are **indicative** and were captured on the maintainer's
> dev box. For PR-relevant comparisons, run on a fixed self-hosted runner —
> cloud CI varies too much for sub-millisecond regressions.

| Fixture     | wiac-core (release) | wiac-core (debug) | viaConstructor (Python) | Notes                |
|-------------|---------------------|-------------------|-------------------------|----------------------|
| simple.dxf  | ~0.63 ms            | TBD               | TBD                     | reference target     |
| nest.dxf    | ~0.92 ms            | TBD               | TBD                     | nested objects       |
| colors.dxf  | ~0.88 ms            | TBD               | TBD                     | many layers          |
| check.dxf   | ~1.50 ms            | TBD               | TBD                     | larger geometry mix  |
| all.dxf     | ~4.40 ms            | TBD               | TBD                     | corpus stress test   |

## Targets

- Rust release ≥ 2× faster than Python on every size.
- Don't gate PRs on perf; do file an issue automatically if Rust regresses
  > 10 % vs the row above.

## How to compare against the Python core

```sh
# Rust:
cargo bench -p wiac-core --bench pipeline -- --quick

# Python (one-shot end-to-end timing):
python -m timeit -n 5 -r 3 \
  -s 'import sys; sys.path.insert(0, "refs/viaconstructor"); from viaconstructor.calc import process' \
  'process("refs/viaconstructor/tests/data/simple.dxf")'
```

Because the Python entry-point isn't a single `process()` call, treat that
snippet as illustrative — wire a thin shim once the rov.16 parity harness
lands and capture both columns from a single command.
