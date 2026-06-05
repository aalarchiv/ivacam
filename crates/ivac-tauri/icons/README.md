# Tauri bundle icons

`tauri.conf.json` references `32x32.png`, `128x128.png`, `128x128@2x.png`,
`icon.icns`, and `icon.ico`. The vector source is `icon.svg`. To regenerate
the raster set, run from `crates/ivac-tauri/`:

```sh
cargo tauri icon icons/icon.svg
```

That command (provided by `tauri-cli`) produces the multi-resolution PNGs
plus the platform-specific `.ico`/`.icns` containers in this directory.
Don't hand-edit the rasters — re-run the icon command instead so they
stay in sync with `icon.svg`.
