import json
from pathlib import Path

path = Path(__file__).with_name("llvm-cov.json")
data = json.loads(path.read_text(encoding="utf-8"))
wanted = (
    "feed_character",
    "apply_backspace",
    "render_preedit",
    "resync_shifted_ascii_from_raw",
)
files = data["data"][0]["files"]
dispatch = next(f for f in files if f["filename"].replace("\\", "/").endswith("dispatch.rs"))
print(f"FILE {dispatch['filename']}")
print(f"functions_field_type={type(dispatch.get('functions')).__name__} len={len(dispatch.get('functions') or [])}")
if dispatch.get("functions"):
    sample = dispatch["functions"][0]
    print(f"sample_keys={list(sample.keys())}")
    print(f"sample_name={sample.get('name')}")

hits = []
for fn in dispatch.get("functions") or []:
    name = fn.get("name") or ""
    if any(needle in name for needle in wanted):
        regions = fn.get("regions") or []
        covered = 0
        for region in regions:
            # llvm-cov JSON region: [line_start, col_start, line_end, col_end, execution_count, ...]
            count = region[4] if len(region) > 4 else 0
            if count > 0:
                covered += 1
        hits.append((name, fn.get("count"), len(regions), covered))

print(f"hit_count={len(hits)}")
for name, count, nregions, covered in hits:
    print(f"FN {name} count={count} regions={nregions} covered_regions={covered}")

summary = dispatch.get("summary") or {}
lines = summary.get("lines") or {}
regions = summary.get("regions") or {}
functions = summary.get("functions") or {}
print(
    "FILE_SUMMARY "
    f"lines={lines.get('covered')}/{lines.get('count')} "
    f"regions={regions.get('covered')}/{regions.get('count')} "
    f"functions={functions.get('covered')}/{functions.get('count')}"
)

# Also print any function whose name contains shift/latin/backspace
extra = []
for fn in dispatch.get("functions") or []:
    name = fn.get("name") or ""
    if any(part in name.lower() for part in ("shift", "latin", "backspace", "resync", "preedit")):
        extra.append(name)
print("extra_names=" + " || ".join(extra[:30]))
