"""Collect upstream license notices from locked, locally resolved dependencies."""
from pathlib import Path
import json
import shutil
import subprocess
import urllib.request

root = Path(__file__).resolve().parents[1]
out = root / "src-tauri" / "licenses"
out.mkdir(parents=True, exist_ok=True)
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--locked", "--format-version", "1", "--manifest-path",
    str(root / "src-tauri" / "Cargo.toml")
], cwd=root, text=True))
index = []
for pkg in metadata["packages"]:
    if pkg["name"] == "qwen-desk":
        continue
    folder = Path(pkg["manifest_path"]).parent
    candidates = [p for p in folder.iterdir() if p.is_file() and p.name.lower().startswith(("license", "licence", "copying", "notice"))]
    target = out / f'{pkg["name"]}-{pkg["version"]}'
    target.mkdir(exist_ok=True)
    for file in candidates:
        shutil.copyfile(file, target / file.name)
    index.append({"name": pkg["name"], "version": pkg["version"], "license": pkg.get("license"), "repository": pkg.get("repository"), "notices": [p.name for p in candidates]})
api = root / "node_modules" / "@tauri-apps" / "api"
if api.exists():
    target = out / "tauri-api-js"
    target.mkdir(exist_ok=True)
    for file in api.iterdir():
        if file.is_file() and file.name.lower().startswith(("license", "licence", "notice")):
            shutil.copyfile(file, target / file.name)
license_url = "https://raw.githubusercontent.com/ggml-org/llama.cpp/b11796/LICENSE"
with urllib.request.urlopen(license_url, timeout=30) as response:
    text = response.read()
if b"MIT License" not in text or len(text) > 20000:
    raise RuntimeError("Unexpected upstream llama.cpp license response")
(out / "llama.cpp-LICENSE").write_bytes(text)
shutil.copyfile(root / "THIRD_PARTY_NOTICES.md", out / "THIRD_PARTY_NOTICES.md")
(out / "index.json").write_text(json.dumps(index, indent=2, ensure_ascii=False), encoding="utf-8")
print(f"Collected notices for {len(index)} resolved Rust packages, frontend API and llama.cpp.")
