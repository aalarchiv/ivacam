# Windows code signing via SignPath Foundation

> **Status: planned, not yet set up.** This is a TODO / runbook for a
> future release — nothing here is active yet. ivaCAM does **not**
> currently sign its Windows installers; testers will hit a SmartScreen
> warning until this is configured. Track it as the plan for when the
> project goes public and a remote is added.

The plan: sign ivaCAM's Windows installers with a **free** OV
code-signing certificate from [SignPath Foundation](https://signpath.org/),
which gives qualifying open-source projects signing infrastructure at no
cost. Signing is what stops Microsoft Defender SmartScreen from scaring
testers off the installer.

This is a runbook for that setup. Several steps are **one-time, external,
and yours to do** (creating the GitHub repo, applying to SignPath, pasting
tokens into GitHub settings) — they need your accounts and can't be
scripted. The CI wiring (`.github/workflows/release-desktop.yml`) is
already in place and stays inert until you switch it on at the end.

## How it fits together

```
git tag v0.1.0  ──►  GitHub Actions (release-desktop.yml)
                       │
                       ├─ build MSI + NSIS .exe on windows-latest (unsigned)
                       ├─ upload as CI artifact  ──►  SignPath HSM signs them
                       └─ attach SIGNED installers to the GitHub Release
```

Two hard prerequisites, both external: **(1)** the repo must be public on
GitHub, and **(2)** SignPath Foundation must approve the project. Approval
takes days to a few weeks, so start it early.

> **Publisher name caveat:** the certificate is issued to *SignPath
> Foundation*, so that — not "ivaCAM" or your name — is the publisher
> shown in the SmartScreen / UAC dialog. This is the trade-off for a free
> cert. If you need your own publisher identity, buy a Certum OSS cert
> instead and wire it via Tauri's `signCommand` (out of scope here).
>
> **Reputation caveat:** a valid signature does not mean *zero* warnings
> on day one. SmartScreen reputation accrues with downloads over time;
> early testers may still see a prompt until it builds.

---

## Step 1 — Put the repo on GitHub (public) · *you*

> ⚠️ **One-way door.** Pushing public exposes all source and the full
> commit history. The pre-public audit
> (`ivac-p11u.1`) found no secrets in the tree or history, bd
> issue text is not tracked, and `refs/` is ignored — so it's safe on
> that front. Be aware that dev-history narration in code comments
> (issue `ivac-62t0`, deferred) and commit messages also
> becomes public.

`gh` is not installed on this machine; create the repo via the web UI or
install the CLI (`sudo apt install gh && gh auth login`).

```sh
# After creating an EMPTY public repo named e.g. "ivacam" on GitHub:
cd /path/to/ivacam
git remote add origin https://github.com/<you>/ivacam.git
git push -u origin main
```

Once a remote exists, restore the `git pull --rebase && git push` step in
the session-completion workflow (see CLAUDE.md — it's currently skipped
because the repo was local-only).

## Step 2 — Apply to SignPath Foundation · *you*

1. Go to <https://signpath.org/> → apply for the open-source program.
2. Provide: the **public repo URL**, the **GPL-3.0-or-later** license
   (already in `LICENSE`), and a short project description (a CAM tool
   that converts DXF/SVG to G-code — see `tauri.conf.json`
   `longDescription`).
3. Review their [OSS terms](https://signpath.org/terms.html) and wait for
   approval (days–weeks).

## Step 3 — Configure the SignPath project · *you, after approval*

In the SignPath web console:

1. Create (or confirm) the **project** for this repo — note its
   **project slug** and your **organization ID**.
2. Create a **signing policy** — start with **`test-signing`** for the
   public beta, switch to **`release-signing`** for real releases (they
   carry different approval rules).
3. Create an **artifact configuration** that unpacks the uploaded zip and
   signs **both** the `.msi` and the NSIS `*-setup.exe`. Note its
   **artifact-configuration slug**.
4. Install the **SignPath GitHub App** on the repo and create a **REST
   API token**.

## Step 4 — Add GitHub Actions secrets & variables · *you*

In the repo: **Settings → Secrets and variables → Actions**.

| Kind | Name | Value |
|------|------|-------|
| Secret | `SIGNPATH_API_TOKEN` | the REST API token from SignPath |
| Variable | `SIGNPATH_ORGANIZATION_ID` | your org GUID |
| Variable | `SIGNPATH_PROJECT_SLUG` | the project slug |
| Variable | `SIGNPATH_SIGNING_POLICY_SLUG` | `test-signing` (then `release-signing`) |
| Variable | `SIGNPATH_ARTIFACT_CONFIGURATION_SLUG` | the artifact-config slug |
| Variable | `SIGNPATH_ENABLED` | `true` — the master switch that activates the signing steps |

Until `SIGNPATH_ENABLED` is `true`, the workflow builds Windows
installers **unsigned** and keeps them only as CI artifacts; nothing is
submitted to SignPath and no installer is attached to public Releases.

> If you keep the repo **private**, also grant the build job
> `permissions: { actions: read, contents: read }` — SignPath's action
> needs them to read job details and download the artifact. Public repos
> with the SignPath GitHub App installed work with the defaults.

## Step 5 — Cut a signed release · *you*

1. **Bump the version** — set `tauri.conf.json` to your release version
   (e.g. `0.1.0` or `0.1.0-beta.1`); it shows in the installer filename
   and the app.
2. Tag and push:
   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```
3. `release-desktop.yml` builds all platforms, routes the Windows
   installers through SignPath, and attaches the **signed** `.msi` +
   `*-setup.exe` to the GitHub Release. Use `workflow_dispatch` first
   (Actions tab → Run workflow) to dry-run the matrix without tagging.

## Verifying a signed installer

On the artifact (or after download):

```powershell
Get-AuthenticodeSignature .\ivaCAM_0.1.0_x64-setup.exe | Format-List
# Status should be 'Valid'; SignerCertificate subject = SignPath Foundation
```

Or right-click the `.exe`/`.msi` → **Properties → Digital Signatures**.

---

See [`BUILDING_WINDOWS.md`](./BUILDING_WINDOWS.md) for building the
installers themselves (native or cross-compiled). The cross-compiled
build **cannot** be signed this way — signing runs in CI against the
native `windows-latest` artifacts.
