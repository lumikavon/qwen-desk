# Releasing Qwen Desk for Windows x64

Release automation targets Windows 10/11 x64, Tauri 2, Node.js 24, and stable Rust with the `x86_64-pc-windows-msvc` toolchain. The application and both installers are currently **unsigned**. Verify the download against `SHA256SUMS` before running it; Windows may show an unknown-publisher or SmartScreen prompt.

## Installers and downloads

A release such as `v1.0.0` contains:

| File | Purpose |
| --- | --- |
| `QwenDesk-1.0.0-x64-no-model-setup.exe` | NSIS application installer; download models later or import existing files. |
| `QwenDesk-1.0.0-x64-with-model-setup.exe` | Inno Setup installer bootstrap for the pinned bundled models. |
| `QwenDesk-1.0.0-x64-with-model-setup-1.bin`, `-2.bin`, etc. | Inno diskspanned data, each at most 1 GiB. Every numbered part is required. |
| `models.lock.json` | Pinned model revisions, filenames, sizes, hashes, and license information. |
| `SHA256SUMS` | SHA-256 hashes of every other release file. |
| `RELEASE-NOTES.md` | Release and installation notes. |

For the with-model installer, download **ALL files matching `QwenDesk-1.0.0-x64-with-model-setup*`**, keeping the `.exe` and every `.bin` together in one folder with their original filenames. Run the `.exe`; do not run or extract the `.bin` files separately. Mixing versions, omitting a part, or allowing a browser to rename a duplicate download will break installation.

Both variants require Microsoft Edge WebView2 and the [Microsoft Visual C++ 2015–2022 x64 Redistributable](https://aka.ms/vs/17/release/vc_redist.x64.exe). Install these prerequisites in advance for an offline setup. The with-model variant installs models offline and can run inference without Hugging Face credentials on a system with those prerequisites ready. GPU inference also requires an appropriate Vulkan driver; CPU inference is available.

The bundled weights are installed in `%LOCALAPPDATA%\com.qwendesk.desktop\models`. Personal models and application settings are preserved on uninstall. When switching between the no-model NSIS installer and the with-model Inno installer, uninstall the previous application first so that two installer technologies do not manage the same application installation. Retained models and settings can be reused after reinstalling.

Application code licensing does not replace the licenses of model weights, runtimes, or other dependencies. Review the model sources and licenses recorded in `models.lock.json`, the accompanying license notices, and `THIRD_PARTY_NOTICES.md` before redistributing bundled weights.

## Local preparation

Use a Windows x64 machine with:

- Git, Windows PowerShell 5.1 or newer, and Node.js **24 x64** with npm.
- Stable Rust MSVC x64, including `rustfmt`; the committed Cargo lockfile must be present.
- Visual Studio 2022 Build Tools with Desktop development with C++, the MSVC x64 compiler, and a Windows SDK.
- WebView2 and the Visual C++ runtime for installation and application validation.
- Permission to prepare the pinned Inno Setup 6 compiler automatically with `scripts/prepare-installer.ps1` in isolated, ignored `.work/tools/inno`. No Chocolatey or machine-wide compiler installation is required.
- Network access to npm, Rust crates, GitHub runtimes/compiler downloads, and Hugging Face models, plus enough free disk space for downloads, extracted runtime files, both installers, and build outputs. Allow at least 20 GiB free and increase it as models grow.

`HF_TOKEN` is optional for public model downloads. If needed for authenticated or gated downloads, set it in the process environment or as a masked CI secret and obtain access from the model publisher. Never put it into command arguments, source files, release notes, or artifacts. The with-model installer does not require an end user's Hugging Face token.

Run the same checks used in CI:

```powershell
npm ci
npm test
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
```

Build both variants:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1 -Variant all -Tag v1.0.0
```

`-Tag` is optional for a local build. When supplied, the build validates it against `package.json`, the Tauri configuration, and `src-tauri/Cargo.toml`; the versions must agree. Packaging prepares the pinned runtimes and models and writes the complete release set to `artifacts/release/`. It does not publish to npm.

Before publication, use a clean checkout of the version tag, including the committed release scripts and lockfiles. The publisher requires the tag to resolve to `HEAD`, checks the provider's remote tag commit, and rejects tracked source changes. A local build without a tag can be inspected, but publishing always requires `-Tag`.

## GitHub Actions

The intended GitHub repository is `lumikavon/qwen-desk`. `.github/workflows/release.yml` runs all checks on pushes to `main`. A push of an existing `v*` version tag runs checks, builds both installers, and publishes the release. Normal branch builds have read-only repository permissions; only the release job receives `contents: write`. No npm publishing or large intermediate artifact archive is involved.

The release job caches `.work/models` by the model-lockfile digest. When the unchanged default models are missing, disposable GitHub-hosted Windows runners can seed that cache from the checksum-pinned `v1.0.0` installer and data files already published in this repository. The manifest and installer parts are verified before extraction, and every extracted weight is checked against the current model lockfile. Local and self-hosted machines never execute the seed installer. The application remains checked out at the requested release tag; the cache helper comes from the workflow commit. Retain the pinned seed release for cache bootstrapping.

To release, update and commit the matching versions and lockfile, push the source commit, then create and push the version tag through your normal review process. For a retry or an existing tag, use **Actions → Windows release → Run workflow**, selecting the workflow from `main` and entering `v1.0.0` in the `tag` input. Both checks and packaging check out that tag. A branch name or a nonexistent tag cannot be published, and the build still validates source versions.

The workflow supplies `GH_TOKEN` from its job token. `HF_TOKEN` can be configured as an optional repository secret. For a manual local publication, install a current [GitHub CLI](https://cli.github.com/) supporting `gh api --slurp` and authenticate with `gh auth login`, or provide `GH_TOKEN` in the environment:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/publish-release.ps1 -Provider github -Tag v1.0.0 -Directory artifacts/release -Repository lumikavon/qwen-desk
```

`-Repository` defaults to `GH_REPO`. The publisher creates a draft, uploads the complete verified file set, checks asset names, count, upload state and sizes, and verifies server SHA-256 digests when available. Only then does it publish the draft. Every individual GitHub asset must be smaller than 2 GiB; the 1 GiB Inno parts keep bundled models within this limit.

If upload fails, the draft remains for retry. Rerunning can replace matching assets **only in that draft**; unexpected existing draft assets cause a failure for review. An already published release is rejected and never silently replaced. If publication succeeded but the final response was lost, inspect the published release before taking further action. Release concurrency is serialized per tag, including tag-push and manual-dispatch runs.

## GitLab CI and Generic Package Registry

`.gitlab-ci.yml` is ready to use in a GitLab project; no GitLab host or project is assumed or created. Register a **Windows x64 shell runner tagged `windows`**, configured for PowerShell and the prerequisites above. Provide Node 24, Git, Rust stable MSVC, `rustfmt`, Visual Studio Build Tools, Windows SDK, and sufficient disk space in the runner account. Permit the pinned compiler preparation during packaging. The runner must have network access and permission to run protected tag pipelines when tags are protected.

Checks run before the release stage. Version-tag pipelines then run `npm ci`, build, package, upload, and create the release in one Windows job, retaining the large files on that job's local disk. Only the small `SHA256SUMS`, notes, and model lockfile are retained as seven-day job artifacts. The installers and `.bin` data are **never uploaded as a job artifact archive**.

Enable the project's Generic Package Registry and allow `CI_JOB_TOKEN` to read package metadata, upload generic packages, read repository tags, and create releases. The built-in token normally works for its own project; self-managed instance policies or fine-grained token settings may require these permissions to be enabled. Cross-project use requires the appropriate job-token allowlist. Registry maximum file sizes, storage quotas, proxy request-size limits, and timeouts must permit individual files up to 1 GiB. Exempt release package versions from cleanup policies so release links remain usable.

The job explicitly invokes:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1 -Variant all -Tag $env:CI_COMMIT_TAG
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/publish-release.ps1 -Provider gitlab -Tag $env:CI_COMMIT_TAG -Directory artifacts/release
```

The publisher obtains `CI_API_V4_URL`, `CI_PROJECT_ID`, `CI_JOB_TOKEN`, and the pipeline commit/tag from the environment. API access uses HTTPS and the token stays in request headers. Each file is uploaded to:

```text
${CI_API_V4_URL}/projects/<escaped-project-id>/packages/generic/qwen-desk/<escaped-tag>/<escaped-filename>
```

Project IDs, tags, and filenames are escaped as individual URL segments. The publisher verifies registry filenames, sizes, and SHA-256 hashes using package metadata, then creates a GitLab Release whose asset links point to these persistent package URLs. Private-project downloads still require normal GitLab access.

A failed upload can be retried for the same version: already uploaded files are reused only when their sizes and hashes match. Unexpected, duplicate, or conflicting package files stop the retry. An existing GitLab Release is rejected, so a retry cannot alter an already published release. Diagnose conflicting package contents before retrying, or choose a new version. Keep Generic Package Registry cleanup separate from the short-lived manifest job artifacts.

## Publisher validation and limits

`scripts/publish-release.ps1` supports Windows PowerShell 5.1. It accepts `-Provider github|gitlab`, required `-Tag`, optional `-Directory` (default `artifacts/release`), and optional GitHub `-Repository` (default `GH_REPO`).

Before any upload it rejects missing or unexpected files, subdirectories, reparse points, unsafe filenames, empty files, noncontiguous data parts, data parts over 1 GiB, malformed or duplicate checksum entries, hash mismatches, and incomplete checksum coverage. `SHA256SUMS` must list every other release file, including notes and the model lockfile. Keep credentials and unrelated files out of the release directory.

Publishing performs real remote writes. Parser checks and local validation can be done without credentials, but verifying provider permissions, package settings, multi-gigabyte transfers, and installer behavior requires an actual tagged CI run and Windows installation testing. This documentation and the configuration alone do not create repositories, tags, or releases.
