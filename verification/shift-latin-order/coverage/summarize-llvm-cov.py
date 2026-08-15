import json
from pathlib import Path

path = Path(__file__).with_name("llvm-cov.json")
data = json.loads(path.read_text(encoding="utf-8"))

wanted = {
    "feed_character": "feed_character",
    "apply_backspace": "apply_backspace",
    "render_preedit": "render_preedit",
    "resync_shifted_ascii_from_raw": "resync_shifted_ascii_from_raw",
}

# Shift-Latin-relevant line ranges in dispatch.rs (inclusive).
# Lines after these bounds are kana / pending-romaji / CJK normalize and
# are out of this campaign's scope.
ARMS = {
    "feed_character": (2278, 2326),
    "apply_backspace": (3502, 3512),
    "render_preedit": (4165, 4193),
    "resync_shifted_ascii_from_raw": (3476, 3495),
}
OUT_OF_SCOPE = {
    "feed_character": (2328, 2368),
    "apply_backspace": (3514, 3565),
    "render_preedit": (4195, 4240),
    "resync_shifted_ascii_from_raw": None,
}


def region_stats(regions, lo=None, hi=None):
    selected = regions
    if lo is not None and hi is not None:
        selected = [region for region in regions if region[0] <= hi and region[2] >= lo]
    covered = sum(1 for region in selected if len(region) > 4 and region[4] > 0)
    return covered, len(selected)


live = {}
for fn in data["data"][0]["functions"]:
    name = fn.get("name") or ""
    for short, needle in wanted.items():
        if needle in name and fn.get("count", 0) > 0:
            regions = fn.get("regions") or []
            covered, total = region_stats(regions)
            arm_lo, arm_hi = ARMS[short]
            arm_covered, arm_total = region_stats(regions, arm_lo, arm_hi)
            mcdc = fn.get("mcdc_records") or []
            live[short] = {
                "count": fn["count"],
                "covered_regions": covered,
                "regions": total,
                "arm_covered": arm_covered,
                "arm_regions": arm_total,
                "mcdc_records": len(mcdc),
                "name": name,
            }

out = Path(__file__).with_name("llvm-cov-report.md")
lines = [
    "# llvm-cov line/region coverage — Shift-Latin production functions",
    "",
    "Tool: `cargo-llvm-cov 0.8.7` + `llvm-tools-preview`.",
    "Command: `rtk cargo llvm-cov -p sakura-engine --lib --json --output-path verification/shift-latin-order/coverage/llvm-cov.json -- shift_latin`.",
    "Filter: `shift_latin` (45 tests after the coverage-neighbor pass). This is **line/region coverage**, not C2 and not MC/DC.",
    "",
    "True C2 / MC/DC of these functions is still impossible from this artifact:",
    f"- `mcdc_records` on the live functions: { {k: v['mcdc_records'] for k, v in live.items()} }.",
    "- Keymap `shift+backspace` lives in TOML (`data/keymap-ms-ime.toml`, `data/keymap-atok.toml`) and has no LLVM counters; it is covered by `contract::keymap_contract_shift_backspace_is_delete_back_while_composing`.",
    "",
    "## Whole-function region coverage",
    "",
    "These percentages stay low because each function also owns kana / pending-romaji / CJK normalize arms. That is not a silent hole: see Out of scope below.",
    "",
    "| function | executions | covered regions | total regions | region % |",
    "|---|---:|---:|---:|---:|",
]
for key in (
    "feed_character",
    "apply_backspace",
    "render_preedit",
    "resync_shifted_ascii_from_raw",
):
    row = live[key]
    pct = 100.0 * row["covered_regions"] / row["regions"] if row["regions"] else 0.0
    lines.append(
        f"| `{key}` | {row['count']} | {row['covered_regions']} | {row['regions']} | {pct:.1f}% |"
    )

lines.extend(
    [
        "",
        "## Shift-Latin-relevant arm region coverage",
        "",
        "Regions whose source span overlaps the Shift-Latin early-return / latch / raw-caret arms.",
        "",
        "| function | arm lines | covered | total | arm region % |",
        "|---|---|---:|---:|---:|",
    ]
)
for key in (
    "feed_character",
    "apply_backspace",
    "render_preedit",
    "resync_shifted_ascii_from_raw",
):
    row = live[key]
    lo, hi = ARMS[key]
    pct = 100.0 * row["arm_covered"] / row["arm_regions"] if row["arm_regions"] else 0.0
    lines.append(
        f"| `{key}` | {lo}–{hi} | {row['arm_covered']} | {row['arm_regions']} | {pct:.1f}% |"
    )

lines.extend(
    [
        "",
        "## Out of scope (kana / CJK / pending romaji)",
        "",
        "These line ranges share the same functions but are not Shift-Latin branches. They are listed so the low whole-function percentages are not a silent hole.",
        "",
        "| function | out-of-scope lines | why |",
        "|---|---|---|",
        "| `feed_character` | 2328–2368 | romaji `table.feed`, decimal-after-digit, kana insert |",
        "| `apply_backspace` | 3514–3565 | pending-romaji / kana-group delete |",
        "| `render_preedit` | 4195–4240 | kana pending + `normalizer.normalize_into` |",
        "| `resync_shifted_ascii_from_raw` | (none) | entire function is Shift-Latin |",
        "",
    ]
)

dispatch = next(
    f
    for f in data["data"][0]["files"]
    if f["filename"].replace("\\", "/").endswith("dispatch.rs")
)
summary = dispatch["summary"]
lines.extend(
    [
        "Whole-file `dispatch.rs` under the same filter (includes unrelated functions):",
        f"- lines {summary['lines']['covered']}/{summary['lines']['count']} ({summary['lines']['percent']:.2f}%)",
        f"- regions {summary['regions']['covered']}/{summary['regions']['count']} ({summary['regions']['percent']:.2f}%)",
        f"- functions {summary['functions']['covered']}/{summary['functions']['count']} ({summary['functions']['percent']:.2f}%)",
        "",
        "Zero-count duplicate symbols for the same names exist in other codegen units and were ignored.",
        "",
    ]
)
out.write_text("\n".join(lines), encoding="utf-8")
print(out.read_text(encoding="utf-8"))
