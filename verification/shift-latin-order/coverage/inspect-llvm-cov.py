import json
from pathlib import Path

path = Path(__file__).with_name("llvm-cov.json")
data = json.loads(path.read_text(encoding="utf-8"))
root = data["data"][0]
print("data_keys", sorted(root.keys()))
print("files", len(root.get("files") or []))
print("functions", len(root.get("functions") or []))
if root.get("functions"):
    sample = root["functions"][0]
    print("fn_keys", sorted(sample.keys()))
    print("fn_sample_name", sample.get("name"))
    print("fn_sample_filenames", sample.get("filenames"))
wanted = (
    "feed_character",
    "apply_backspace",
    "render_preedit",
    "resync_shifted_ascii_from_raw",
)
hits = []
for fn in root.get("functions") or []:
    name = fn.get("name") or ""
    if any(needle in name for needle in wanted):
        regions = fn.get("regions") or []
        covered = sum(1 for region in regions if len(region) > 4 and region[4] > 0)
        hits.append((name, fn.get("count"), len(regions), covered, fn.get("filenames")))
print("hit_count", len(hits))
for item in hits:
    print("FN", item)
