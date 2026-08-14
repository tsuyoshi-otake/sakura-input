# Release model artifacts

`sakura-rerank-tiny-v1/` contains the self-authored Sakura reranker artifact
that is packaged by the normal installer. The ONNX file is committed so a
tagged Sakura Input checkout can reproduce its installer without relying on an
unversioned sibling checkout or mutable network model download.

The source research manifest remains byte-identical evidence of the training
run. It records the quality-gate state at training time. Product distribution
authorization and the MIT license are expressed separately in the generated
runtime manifest; the build must not rewrite the measured Gate A result.

Import or refresh these two files only through
`scripts/import-sakura-rerank-release-model.ps1`, which validates their exact
byte lengths and SHA-256 values before copying them.
