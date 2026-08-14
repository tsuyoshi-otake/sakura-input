# Third-party notices

Sakura Input bundles derived dictionary data from the following pinned
sources. `data/SOURCES.lock` is the machine-readable source of truth for the
repository URLs, revisions, source paths, and transformation policy.

| Component | Use | License notice |
|---|---|---|
| Google Mozc OSS dictionary | Base Japanese lexicon and connection costs | `THIRD_PARTY_LICENSES/mozc-dictionary.txt` |
| smile-chat public Japanese glossary | IT-domain surface, reading, alias, domain, and description overlay | `THIRD_PARTY_LICENSES/smile-chat-public-MIT.txt` |
| Japanese WordNet 1.1 (NICT / Francis Bond and contributors) | Optional Japanese definitions and explicit WordNet relations in the generated dictionary detail table | `THIRD_PARTY_LICENSES/japanese-wordnet-1.1-NICT.txt` |
| Microsoft ONNX Runtime 1.28.0 | Out-of-process execution of the bundled Sakura reranker | Installed as `licenses/onnxruntime-MIT.txt` and `licenses/onnxruntime-ThirdPartyNotices.txt` |

The Mozc dictionary is a mixed-license data set rather than a BSD-only work.
The bundled notice preserves Google BSD-3-Clause terms, the IPAdic/ICOT
conditions, and the Okinawa dictionary Public Domain notice. The smile-chat
files used here are below the repository's `frontend/public/LICENSE` boundary
and are MIT licensed.

Japanese WordNet data is separate third-party dictionary data, not Sakura's
MIT-licensed program code. Its NICT license permits use, modification, and
distribution provided that its copyright, disclaimer, and license statements
are preserved; it does not grant endorsement or publicity use of NICT's name.

The normal installer also bundles Microsoft ONNX Runtime 1.28.0 beside the
out-of-process neural worker. The release staging process copies the exact MIT
license and third-party notices from the pinned official ONNX Runtime archive
into the installed `licenses` directory.

