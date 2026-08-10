"""Export the pinned long-conversion masked-language model to ONNX.

This script is build-time only. The installed IME never imports Python,
PyTorch, Transformers, Hugging Face Hub, or Optimum.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

from huggingface_hub import snapshot_download


MODEL_ID = "ku-nlp/deberta-v2-tiny-japanese-char-wwm"
MODEL_REVISION = "41bcb8a393383a039c7ee18ded6893ca82e668b7"
ONNX_OPSET = 18
ONNX_OPTIMIZATION = "O2"
ONNXRUNTIME_VERSION = "1.28.0"
REQUIRED_SPECIAL_TOKENS = ("[PAD]", "[CLS]", "[SEP]", "[UNK]", "[MASK]")


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path: pathlib.Path) -> dict[str, object]:
    with path.open("r", encoding="utf-8") as source:
        value = json.load(source)
    if not isinstance(value, dict):
        raise RuntimeError(f"expected a JSON object in {path}")
    return value


def validate_tokenizer(source_directory: pathlib.Path) -> None:
    config = read_json(source_directory / "tokenizer_config.json")
    if config.get("tokenizer_class") != "BertJapaneseTokenizer":
        raise RuntimeError("pinned artifact is not BertJapaneseTokenizer")
    if config.get("subword_tokenizer_type") != "character":
        raise RuntimeError("pinned artifact does not use character tokenization")
    if config.get("word_tokenizer_type") != "basic":
        raise RuntimeError("pinned artifact does not use the basic word tokenizer")

    vocabulary = (source_directory / "vocab.txt").read_text(encoding="utf-8-sig").splitlines()
    missing = [token for token in REQUIRED_SPECIAL_TOKENS if token not in vocabulary]
    if missing:
        raise RuntimeError(f"pinned vocabulary is missing special tokens: {missing}")


def write_manifest(output_directory: pathlib.Path) -> None:
    files = []
    for name in ("model.onnx", "vocab.txt"):
        path = output_directory / name
        files.append({"path": name, "bytes": path.stat().st_size, "sha256": sha256(path)})
    manifest = {
        "schema_version": 1,
        "model": {
            "id": MODEL_ID,
            "revision": MODEL_REVISION,
            "format": "onnx-fp32-o2",
            "opset": ONNX_OPSET,
        },
        "tokenizer": {
            "class": "BertJapaneseTokenizer",
            "word_tokenizer_type": "basic",
            "subword_tokenizer_type": "character",
            "do_lower_case": False,
        },
        "runtime": {"name": "onnxruntime", "version": ONNXRUNTIME_VERSION},
        "files": files,
    }
    target = output_directory / "manifest.json"
    temporary = output_directory / ".manifest.json.tmp"
    temporary.write_text(
        json.dumps(manifest, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    os.replace(temporary, target)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-directory", required=True, type=pathlib.Path)
    parser.add_argument("--output-directory", required=True, type=pathlib.Path)
    args = parser.parse_args()

    args.output_directory.mkdir(parents=True, exist_ok=True)
    snapshot_download(
        repo_id=MODEL_ID,
        revision=MODEL_REVISION,
        local_dir=args.source_directory,
        allow_patterns=[
            "config.json",
            "model.safetensors",
            "special_tokens_map.json",
            "tokenizer_config.json",
            "vocab.txt",
        ],
    )
    validate_tokenizer(args.source_directory)

    # Export into a sibling temporary directory. This prevents Optimum from
    # consuming stale output artifacts and lets the installed model directory
    # expose exactly the versioned model, vocabulary, and manifest contract.
    temporary_directory = pathlib.Path(
        tempfile.mkdtemp(prefix="sakura-neural-export-", dir=args.output_directory.parent)
    )
    staged_paths: list[pathlib.Path] = []
    try:
        command = [
            sys.executable,
            "-m",
            "optimum.exporters.onnx",
            "--model",
            str(args.source_directory),
            "--task",
            "fill-mask",
            "--opset",
            str(ONNX_OPSET),
            "--optimize",
            ONNX_OPTIMIZATION,
            str(temporary_directory),
        ]
        subprocess.run(command, check=True)

        required = [temporary_directory / "model.onnx", args.source_directory / "vocab.txt"]
        missing = [str(path) for path in required if not path.is_file()]
        if missing:
            raise RuntimeError(f"ONNX export did not create required artifacts: {missing}")

        import onnx

        onnx.checker.check_model(str(temporary_directory / "model.onnx"))
        for name, source in (("model.onnx", temporary_directory / "model.onnx"),
                             ("vocab.txt", args.source_directory / "vocab.txt")):
            destination = args.output_directory / name
            stage = args.output_directory / f".{name}.tmp"
            staged_paths.append(stage)
            shutil.copyfile(source, stage)
            os.replace(stage, destination)
        write_manifest(args.output_directory)
    finally:
        for stage in staged_paths:
            stage.unlink(missing_ok=True)
        shutil.rmtree(temporary_directory, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
