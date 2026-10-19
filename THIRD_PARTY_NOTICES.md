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
| Japanese Wikipedia text (Wikipedia contributors) | Training text for the bundled Sakura reranker model; no Wikipedia text is shipped | CC BY-SA 4.0, see the attribution below |

The Mozc dictionary is a mixed-license data set rather than a BSD-only work.
The bundled notice preserves Google BSD-3-Clause terms, the IPAdic/ICOT
conditions, and the Okinawa dictionary Public Domain notice. The smile-chat
files used here are below the repository's `frontend/public/LICENSE` boundary
and are MIT licensed.

The smile-chat Japanese glossary was written by the owner of this repository,
tsuyoshi-otake, while smile-chat was distributed under the MIT License, and is
used here under that license. The copyright line in
`THIRD_PARTY_LICENSES/smile-chat-public-MIT.txt` is the one in smile-chat's
`frontend/public/LICENSE` file and names the author of that file, not the
author of the glossary.

Japanese WordNet data is separate third-party dictionary data, not Sakura's
MIT-licensed program code. Its NICT license permits use, modification, and
distribution provided that its copyright, disclaimer, and license statements
are preserved; it does not grant endorsement or publicity use of NICT's name.

The normal installer also bundles Microsoft ONNX Runtime 1.28.0 beside the
out-of-process neural worker. The release staging process copies the exact MIT
license and third-party notices from the pinned official ONNX Runtime archive
into the installed `licenses` directory.

## Sakura reranker model training data

The bundled Sakura reranker model (`neural/sakura-rerank-tiny-v1/model.onnx`)
is the repository's own model and is distributed under the MIT License, as
approved by the repository owner on 2026-08-14. It was trained in the
separate `sakura-rerank` research repository on text spans selected from the
Japanese Wikipedia database dump of 2026-08-01
(`jawiki-20260801-pages-articles-multistream`,
https://dumps.wikimedia.org/jawiki/20260801/). Candidate training rows were
screened by an OpenAI GPT model acting as an automated reviewer. The model
contains learned weights only; no Wikipedia text is included in this
repository or in the installer.

Attribution: the training text is from Japanese Wikipedia, written by
Wikipedia contributors (https://ja.wikipedia.org/), and is available under the
Creative Commons Attribution-ShareAlike 4.0 International License
(https://creativecommons.org/licenses/by-sa/4.0/) and the GNU Free
Documentation License. Whether these terms apply to a model trained on the
text is not settled; this attribution is given so the training source is
known.
