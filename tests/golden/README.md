# Golden-file corpus

Each test fixture in `refs/viaconstructor/tests/data/*.dxf` gets a paired
`<name>.<setup>.expected.gcode` produced by the upstream Python tool. The
Rust core's output is diffed against those files with a tolerant comparator
(see `wiac_core::testing::diff_gcode`) on every `cargo test`.

The integration tests live in `crates/wiac-core/tests/golden_corpus.rs`.

## Refresh references

After a behavioral change in the Python tool, regenerate the references:

```bash
just refresh-golden            # iterates the corpus + setup matrix
```

This invokes the Python `viaConstructor` headless-mode entry point against
each input file under several setup configurations and writes the gcode
into `tests/golden/expected/`.

## Setup matrix

For now we run a small matrix that exercises the implemented Rust paths:

* `default`     — outside offset, mill mode, 3mm tool, depth=-2, step=-1
* `inside`      — same with `mill.offset = inside`
* `pocket`      — closed contours pocketed via `pockets.active = true`

Additional setups land alongside the gcode features they exercise.
