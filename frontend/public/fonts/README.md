# Bundled fonts

These ship with the AddTextDialog so the user can stamp text into a project
without first locating a font on disk.

## DejaVuSans.ttf (filled-outline)

DejaVu Sans is a Vera-derivative font under the
[Bitstream Vera license](https://dejavu-fonts.github.io/License.html) — a
permissive BSD-style license that allows free redistribution and bundling.
Used for Profile / Pocket / Carve / Outline styles.

## Engraving / single-line font (slot reserved)

A bundled engraving (Hershey-style) font is tracked in the issue queue —
license-vetting takes longer than the rt1.5 budget allowed. Until then,
users can supply their own single-line TTF (RhSS, OSIFont, etc.) via the
dialog's file picker; the `is_single_line_font` heuristic auto-detects
the family and the dialog drives the Engraving warning chip accordingly.
