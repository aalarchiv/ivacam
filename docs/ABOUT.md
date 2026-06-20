ivaCAM by Soenke J. Peters is an open-source CAM for hobbyist CNC, laser cutters, drag-knife plotters, and plasma CNC machines.

<!--
  This file is the source for the About dialog's prose. It is read at
  build time by the `ivac-about-md` plugin in `frontend/vite.config.ts`,
  which substitutes the tokens below and exposes the result as the
  `virtual:about` module. Only a subset of Markdown is rendered (headings,
  bold, italic, inline code, links, bullet lists, paragraphs) — see
  `frontend/src/lib/components/markdown-lite.ts`. Tokens:
    %%VERSION%%      git describe --always --dirty (e.g. v0.1.0-3-gabc1234)
    %%PKG_VERSION%%  package.json version (semver)
    %%DATE%%         ISO-8601 UTC build timestamp
-->

### License

ivaCAM is distributed under the [GNU General Public License v3.0 or later](https://www.gnu.org/licenses/gpl-3.0.html). The source for this build (`%%VERSION%%`) is at [github.com/aalarchiv/ivacam](https://github.com/aalarchiv/ivacam).

The bundled license text ships in the repository as `LICENSE`; you also have a copy in the install directory of your desktop build.

### Acknowledgements

- **[viaConstructor](https://github.com/multigcs/viaconstructor)** — a Python CAM tool of similar scope that inspired the idea and the name (besides being a funny acronym for "I vibe-coded a CAM"). ivaCAM ported some calculations, so it may or may not be some kind of derivative work.
- **Estlcam** — its feature catalogue inspired the CAM primitives ivaCAM implements. No Estlcam code is used; algorithms are implemented from public literature. Estlcam is not free, but it is great software at a reasonable price. Buy it!
- **The CNC and maker community** — bug reports, test geometries, and the machine quirks that turned synthetic test suites into real shop-floor coverage.
