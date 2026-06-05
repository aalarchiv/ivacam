# ivac-core pipeline baseline

End-to-end time per fixture: DXF import → segments → objects → parallel
offset → linuxcnc gcode emit. Captured by
`cargo bench -p ivac-core --bench pipeline`.

> Numbers below are **indicative** and were captured on the maintainer's
> dev box. For PR-relevant comparisons, run on a fixed self-hosted runner —
> cloud CI varies too much for sub-millisecond regressions.

| Fixture     | ivac-core (release) | Notes                |
|-------------|---------------------|----------------------|
| simple.dxf  | ~0.63 ms            | reference target     |
| nest.dxf    | ~0.92 ms            | nested objects       |
| colors.dxf  | ~0.88 ms            | many layers          |
| check.dxf   | ~1.50 ms            | larger geometry mix  |
| all.dxf     | ~4.40 ms            | corpus stress test   |

## Targets

- Don't gate PRs on perf. Do open a bd issue if any row regresses
  > 10 % vs the baseline above between two consecutive `cargo bench`
  runs on the same machine.

## How to run

```sh
cargo bench -p ivac-core --bench pipeline -- --quick
```
