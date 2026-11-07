"""Generate the Issue #440 worker parity fixture from the research implementation.

Run from any directory with a Python that has numpy and onnxruntime:

    python -I generate_issue440_parity.py <sakura-rerank checkout> <student-fp32.onnx> <output.json>

The research checkout must contain the hash-pinned `research_tiny.py` and
`research_tiny_r2.py` used by sakura-rerank Issue #29. Inputs are synthetic
surfaces and costs written here, never dataset rows. The product protocol v1
carries no reading or left context and no segment/system-origin facts, so
features 2, 4 and 5 are zero and the reading/context tensors are empty, exactly
as `sakura_scorer.rs` builds them.
"""
import hashlib
import json
import math
import sys
from pathlib import Path

import numpy as np

RESEARCH = Path(sys.argv[1]).resolve()
MODEL = Path(sys.argv[2]).resolve()
OUTPUT = Path(sys.argv[3])
PACKAGE = RESEARCH / "src" / "sakura_rerank"

# Both research modules supply tensor dimensions, character ids and the
# feature/gate code (sakura-rerank v0.1.0-research.3), so both are verified
# before either is imported.
PINS = {
    PACKAGE / "research_tiny.py":
        "a7ca1007ad2981f3008fa9363723f3d5c232213208ee0a4a9f89e572af3b2035",
    PACKAGE / "research_tiny_r2.py":
        "c2ede3575722b0cda97e0da52a04f42c2402db9cf12aec077d63993f09a95f66",
    MODEL: "a58ebea1257cdc2507b02acb8b7e958181f05577fb629d7928eb1f02fe078686",
}
for path, expected in PINS.items():
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise SystemExit(f"pin mismatch: {path.name}")

sys.path.insert(0, str(RESEARCH / "src"))
from sakura_rerank import research_tiny as v1  # noqa: E402
from sakura_rerank import research_tiny_r2 as r2  # noqa: E402

TAU = 1.7
I32_MIN, I32_MAX = -(2**31), 2**31 - 1
GROUPS = [
    ["機械", "機会", "奇怪", "器械"],
    ["構成", "校正", "公正", "更生", "厚生", "恒星"],
    ["意志", "意思", "医師", "石", "遺志"],
    ["鑑賞", "干渉", "感傷", "観賞", "緩衝"],
    ["日本語", "日本人"],
    ["保証", "保障", "補償"],
    ["追求", "追及", "追究"],
    ["git commit", "ギットコミット", "ｇｉｔ　ｃｏｍｍｉｔ"],
    ["東京都庁", "東京特許許可局長今日急遽休暇許可拒否", "とうきょう"],
    ["😀顔", "顔😀", "A"],
    ["回答", "解答", "快刀", "解凍", "会頭", "回頭"],
    ["生産性の向上", "生産性の工場", "生産性の公序"],
]
COST_PATTERNS = [
    lambda n: [1000 + 120 * i for i in range(n)],
    lambda n: [5000] * n,
    lambda n: [3000 + 5 * i for i in range(n)],
    lambda n: [800 + 2500 * i for i in range(n)],
    lambda n: [4000, 3900] + [4100 + 50 * i for i in range(n - 2)],
    lambda n: [I32_MIN] + [I32_MAX] * (n - 1),
]


def encode(candidates):
    count = len(candidates)
    rows = [{"surface": c["text"], "local_cost": c["local_cost"], "segments": [], "source_category": ""}
            for c in candidates]
    features = np.zeros((v1.TOP_K, r2.FEATURE_DIM), dtype=np.float32)
    features[:count] = np.asarray(r2.candidate_features("", rows), dtype=np.float32)
    features[:, [2, 4, 5]] = 0.0
    ids = np.zeros((v1.TOP_K, v1.SURFACE_LENGTH), dtype=np.int64)
    lengths = np.zeros(v1.TOP_K, dtype=np.int64)
    mask = np.zeros(v1.TOP_K, dtype=bool)
    for index, candidate in enumerate(candidates):
        ids[index], lengths[index] = v1.character_ids(candidate["text"], v1.SURFACE_LENGTH)
        mask[index] = True
    return {
        "context_ids": np.zeros((1, v1.CONTEXT_LENGTH), dtype=np.int64),
        "context_lengths": np.zeros(1, dtype=np.int64),
        "reading_ids": np.zeros((1, v1.READING_LENGTH), dtype=np.int64),
        "reading_lengths": np.zeros(1, dtype=np.int64),
        "candidate_ids": ids[None],
        "candidate_lengths": lengths[None],
        "features": features[None],
        "candidate_mask": mask[None],
    }


session = r2._cpu_session(MODEL)
cases = []
for group_index, group in enumerate(GROUPS):
    for pattern_index, pattern in enumerate(COST_PATTERNS):
        for rotation in range(len(group)):
            surfaces = group[rotation:] + group[:rotation]
            costs = pattern(len(surfaces))
            candidates = [{"fingerprint": 1 + i, "local_cost": int(cost), "text": text}
                          for i, (text, cost) in enumerate(zip(surfaces, costs))]
            feeds = encode(candidates)
            scores = session.run(["scores"], feeds)[0][0].astype(np.float32)
            count = len(candidates)
            z = scores[:count].astype(np.float64)
            top = int(z.argmax())
            margin = float(z[top] - z[0])
            gated = top if top != 0 and margin > TAU else 0
            cases.append({
                "candidates": candidates,
                "features_f32_bits": [int(v) for v in feeds["features"][0].view(np.uint32).reshape(-1)],
                "candidate_ids": [int(v) for v in feeds["candidate_ids"][0].reshape(-1)],
                "candidate_lengths": [int(v) for v in feeds["candidate_lengths"][0]],
                "scores": [float(v) for v in scores[:count]],
                "argmax": top,
                "margin_over_first": margin,
                "gated_choice_with_local_first": gated,
            })
del session

passes = sum(1 for case in cases if case["gated_choice_with_local_first"] != 0)
fixture = {
    "fixture_kind": "sakura_neural_worker_issue440_parity",
    "schema_version": 1,
    "model_sha256": PINS[MODEL],
    "research_r2_sha256": PINS[RESEARCH / "src" / "sakura_rerank" / "research_tiny_r2.py"],
    "gate": {"score": "logit_margin_over_local_choice", "tau": TAU},
    "case_count": len(cases),
    "gate_override_count": passes,
    "cases": cases,
}
OUTPUT.write_text(json.dumps(fixture, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n",
                  encoding="utf-8", newline="\n")
print({"cases": len(cases), "gate_overrides": passes,
       "min_abs_margin_minus_tau": min(abs(c["margin_over_first"] - TAU) for c in cases),
       "bytes": OUTPUT.stat().st_size})
