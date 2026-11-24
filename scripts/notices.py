#!/usr/bin/env python3
"""Collect the locked Linux dependency licenses for a binary distribution."""
import json
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", "x86_64-unknown-linux-gnu"], cwd=root))
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
todo = [meta["resolve"]["root"]]
seen = set()
while todo:
    key = todo.pop()
    if key in seen:
        continue
    seen.add(key)
    todo.extend(dep["pkg"] for dep in nodes[key]["deps"] if any(kind["kind"] != "dev" for kind in dep["dep_kinds"]))

def licenses(directory):
    return [p for p in sorted(directory.iterdir()) if p.is_file() and p.name.lower().startswith(
        ("license", "licence", "copying", "copyright", "notice", "third_party_notice")) and p.suffix not in (".rs", ".toml")]

print("Third-party notices for the x86_64 Linux build. Build-time crates are included.")
for p in sorted((packages[k] for k in seen if k != meta["resolve"]["root"]), key=lambda p: (p["name"],p["version"])):
    base = Path(p["manifest_path"]).parent
    files = licenses(base)
    if p["license_file"]:
        files.append(base / p["license_file"])
    fallback = {"accesskit":"accesskit", "accesskit_unix":"accesskit", "accesskit_consumer":"accesskit",
                "accesskit_atspi_common":"accesskit", "profiling":"profiling", "taffy":"taffy", "spirv":"rspirv"}
    if not files and p["name"] in fallback:
        files = licenses(root/"packaging/licenses"/fallback[p["name"]])
    if not files and p["source"].startswith("git+"):
        top = Path(subprocess.check_output(["git", "-C", str(base), "rev-parse", "--show-toplevel"], text=True).strip())
        files = licenses(top)
    if p["name"] == "rune":
        files.extend(sorted((base/"assets").glob("*LICENSE*")))
    if not files:
        raise SystemExit(f"Missing license text for {p['name']} {p['version']}")
    print(f"\n=== {p['name']} {p['version']} — {p['license']} ===")
    for item in dict.fromkeys(files):
        print(f"\n[{item.name}]\n{item.read_text(errors='replace').strip()}")
