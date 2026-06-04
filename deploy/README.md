# Deploying the web build

The production web build is a **fully static, serverless bundle** — it runs
the CAM engine in the browser via WebAssembly, so there is no API server,
database, or backend to deploy. (Do **not** expose `wiac-server`; it isn't
needed, and the dev-only Vite proxy doesn't apply to a production build.)

## Build

```bash
cd frontend && pnpm build      # → frontend/dist/  (~5 MB, incl. a 2.9 MB .wasm)
```

Build from a **clean checkout** so the About-dialog version stamp
(`git describe --always --dirty`) reads a clean commit rather than `…-dirty`.

Then upload the **entire `frontend/dist/` tree** (it includes `index.html`,
hashed `assets/`, `fonts/`, `samples/`, icons) to your host's web root.

## What the server must do

1. **Serve `.wasm` as `application/wasm`.** The one hard requirement. Modern
   hosts and nginx ≥ 1.21.1 do this automatically; the configs here make it
   explicit anyway.
2. **Serve from the domain root.** Vite `base` is `/`. To host under a
   subpath, rebuild with `base` set or assets will 404.
3. **Nothing special otherwise:** the wasm is single-threaded, so **no
   COOP/COEP / cross-origin-isolation** headers are needed, and there's **no
   client-side router**, so no SPA rewrite rules are required (the optional
   `index.html` fallback in these configs just keeps a stray refresh/deep
   link from 404ing).
4. **HTTPS recommended, not required.** The app runs over plain HTTP; the
   only secure-context dependency is the two "copy to clipboard" buttons
   (About version, error details), which no-op on non-localhost HTTP.

## Configs

| File          | Use                                                              |
|---------------|------------------------------------------------------------------|
| `Caddyfile`   | Caddy — automatic HTTPS, simplest. `caddy run --config deploy/Caddyfile` |
| `nginx.conf`  | nginx server block — drop in `conf.d/`, then `certbot --nginx` for TLS   |

Both default to `server_name cnc.example.com` and `root
/srv/wiaconstructor/dist` — adjust to your domain and path.

## Managed hosts

Netlify / Vercel / Cloudflare Pages / GitHub Pages also work: publish dir
`frontend/dist`, build command `pnpm build`. MIME and HTTPS are handled for
you. For GitHub Pages under a project subpath, remember to set Vite `base`.
