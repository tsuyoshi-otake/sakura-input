# Third-party notices

Sakura Input bundles derived dictionary data from the following pinned
sources. `data/SOURCES.lock` is the machine-readable source of truth for the
repository URLs, revisions, source paths, and transformation policy.

| Component | Use | License notice |
|---|---|---|
| Google Mozc OSS dictionary | Base Japanese lexicon and connection costs | `THIRD_PARTY_LICENSES/mozc-dictionary.txt` |
| smile-chat public Japanese glossary | IT-domain surface, reading, alias, domain, and description overlay | `THIRD_PARTY_LICENSES/smile-chat-public-MIT.txt` |
| Japanese WordNet 1.1 (NICT / Francis Bond and contributors) | Optional Japanese definitions and explicit WordNet relations in the generated dictionary detail table | `THIRD_PARTY_LICENSES/japanese-wordnet-1.1-NICT.txt` |
| Kyoto University NLP `ku-nlp/deberta-v2-tiny-japanese-char-wwm`, revision `41bcb8a393383a039c7ee18ded6893ca82e668b7` | Optional local long-conversion reranker; ONNX-converted model weights and tokenizer vocabulary | `THIRD_PARTY_LICENSES/ku-nlp-deberta-v2-tiny-japanese-char-wwm.txt` (CC BY-SA 4.0) |

The Mozc dictionary is a mixed-license data set rather than a BSD-only work.
The bundled notice preserves Google BSD-3-Clause terms, the IPAdic/ICOT
conditions, and the Okinawa dictionary Public Domain notice. The smile-chat
files used here are below the repository's `frontend/public/LICENSE` boundary
and are MIT licensed.

Japanese WordNet data is separate third-party dictionary data, not Sakura's
MIT-licensed program code. Its NICT license permits use, modification, and
distribution provided that its copyright, disclaimer, and license statements
are preserved; it does not grant endorsement or publicity use of NICT's name.

The optional neural artifact is not part of the Sakura Input MIT-licensed
program code. Sakura Input distributes an ONNX conversion of the pinned Kyoto
University NLP model together with its vocabulary. The conversion is Adapted
Material for the purposes of CC BY-SA 4.0: attribution, the CC BY-SA 4.0 notice,
and the ShareAlike terms apply to redistribution of that converted artifact. The
model notice identifies the authors, source, pinned revision, changes, and
license URL. It does not imply endorsement by Kyoto University NLP. ONNX Runtime
has its own MIT license and third-party notices in the release payload.
