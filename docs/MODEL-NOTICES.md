# Bundled model notices

The **with-model** installer redistributes two GGUF weight files under the Apache License 2.0. The **no-model** installer contains no model weights. Both variants include the llama.cpp CPU and Vulkan runtimes and their separate license notices.

| Model | Distributor and pinned source | Format | License |
| --- | --- | --- | --- |
| Qwen3-4B-Instruct-2507 | [Qwen](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507), quantized by [Unsloth](https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/tree/a06e946bb6b655725eafa393f4a9745d460374c9) | Q4_K_M GGUF | Apache-2.0 |
| Qwen3-Embedding-0.6B | [Qwen official GGUF](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/tree/370f27d7550e0def9b39c1f16d3fbaa13aa67728) | Q8_0 GGUF | Apache-2.0 |

Copyright and attribution belong to the respective model authors and distributors. These files retain their upstream names. Quantized weights are derived representations of the upstream models. Qwen Desk does not modify the distributed GGUF bytes. The generation model is an alternative quantization of the same model family; it is not the AWQ file named by the application's compatibility alias.

`models.lock.json` records the exact repositories, immutable revisions, filenames, sizes and SHA-256 checksums. The build verifies every weight before packaging. Each model's original README is included as `MODEL-CARD.md` from that revision, alongside `LICENSE-APACHE-2.0.txt`. Consult the model cards for capabilities, limitations, attribution and model-specific information. No Hugging Face token or developer configuration is distributed.

The installer places weights in the current user's `%LOCALAPPDATA%\com.qwendesk.desktop\models` directory. Existing weight files and personal settings are preserved, including on uninstall. To replace a different or corrupt existing file, remove it through the application's model manager before reinstalling the bundled weights.
