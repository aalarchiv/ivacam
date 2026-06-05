ivaCAM is open-source CAM for hobbyist CNC, laser, and drag-knife machines.

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

- **[viaConstructor](https://github.com/multigcs/viaconstructor)** — the project that gave the idea. A Python CAM tool with a similar scope; ivaCAM reuses none of its code but stands on the shoulders of its UX exploration.
- **Estlcam** — its feature catalogue inspired the CAM primitives ivaCAM implements. No Estlcam code is used; algorithms are implemented from public literature. Estlcam is not free, but it is great software at a reasonable price. Buy it!
- **The CNC and maker community** — bug reports, test geometries, and the machine quirks that turned synthetic test suites into real shop-floor coverage.
