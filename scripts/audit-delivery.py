"""Audit tracked source (and optional ZIP) without printing secret values."""
from pathlib import Path
import argparse
import json
import re
import subprocess
import zipfile

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument("--zip", type=Path)
parser.add_argument("--directory", action="append", type=Path, default=[], help="Audit an installed or extracted delivery tree")
parser.add_argument("--sensitive-directory", action="append", type=Path, default=[], help="Scan logs or build output for secrets without applying delivery filename exclusions")
args = parser.parse_args()
tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
forbidden = {"huggingface.yml", "huggingface.yaml", ".env"}
private = root / "huggingface.yml"
# Extract only high-entropy HF tokens, never print the contents or include the private file.
secrets = re.findall(rb"hf_[A-Za-z0-9]{20,}", private.read_bytes()) if private.exists() else []
violations = []
count = 0

def check(name, data, delivery=True):
    global count
    count += 1
    parts = Path(name).parts
    if delivery and (Path(name).name in forbidden or any(p in {"node_modules", "target", ".work", ".git"} for p in parts) or name.lower().endswith((".gguf", ".safetensors", ".part", ".log", ".pfx", ".pem"))):
        violations.append({"file": name, "reason": "forbidden delivery file"})
    if any(secret in data or secret.decode('ascii').encode('utf-16le') in data for secret in secrets):
        violations.append({"file": name, "reason": "private token detected"})
    if re.search(rb"hf_[A-Za-z0-9]{30,}", data):
        violations.append({"file": name, "reason": "possible access token"})

for name in tracked:
    if name and (root / name).is_file():
        check(name, (root / name).read_bytes())
# Inspect every historical blob, because ignored files can still have been committed earlier.
objects = subprocess.check_output(["git", "rev-list", "--objects", "--all"], cwd=root).decode().splitlines()
for item in objects:
    oid, _, name = item.partition(" ")
    if not name:
        continue
    kind = subprocess.check_output(["git", "cat-file", "-t", oid], cwd=root).strip()
    if kind == b"blob":
        data = subprocess.check_output(["git", "cat-file", "blob", oid], cwd=root)
        check(f"history/{oid[:12]}/{name}", data)
if args.zip:
    with zipfile.ZipFile(args.zip) as archive:
        for entry in archive.infolist():
            if not entry.is_dir():
                check(entry.filename, archive.read(entry))
for folder, delivery in [(p, True) for p in args.directory] + [(p, False) for p in args.sensitive_directory]:
    if not folder.is_dir():
        raise SystemExit("Audit directory does not exist")
    for file in folder.rglob("*"):
        if file.is_file():
            check(str(file.relative_to(folder)), file.read_bytes(), delivery)
print(json.dumps({"files_checked": count, "git_history_checked": True, "private_token_patterns_loaded": len(secrets), "violations": violations}, indent=2))
raise SystemExit(1 if violations else 0)
