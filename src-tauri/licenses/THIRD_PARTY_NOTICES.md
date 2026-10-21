# Third-party components

Qwen Desk source is licensed under MIT. Bundled third-party components retain their original licenses.

| Component | Version / source | License |
| --- | --- | --- |
| llama.cpp native CPU/Vulkan runtime | b11796, commit 0a1ef309c; https://github.com/ggml-org/llama.cpp | MIT |
| LLVM OpenMP runtime | supplied in the pinned llama.cpp release | Apache-2.0 with LLVM exception; bundled `LICENSE-LLVM-OpenMP` |
| Tauri and official plugins | exact versions in `src-tauri/Cargo.lock`; https://github.com/tauri-apps | MIT / Apache-2.0 |
| Rust dependencies | exact versions in `src-tauri/Cargo.lock` | See each package's license and the bundled license collection |
| @tauri-apps/api | exact version in `package-lock.json` | MIT / Apache-2.0 |

The native archives and their SHA-256 digests are pinned in `scripts/prepare-runtime.ps1`. The installer includes the runtime license text. `scripts/collect-licenses.py` gathers license files from the resolved Cargo packages and the installed frontend runtime package into `src-tauri/licenses`, which is included in the installer. Build dependencies and development utilities are not executed by the installed application.

Default model references point to Qwen3 model families. Model weights are downloaded separately after installation and are not part of this source archive or installer. Consult the respective Hugging Face repositories for model-specific terms, attribution and upstream model cards.
