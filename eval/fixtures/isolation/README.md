Isolated Codex working directories must contain only case.json and
result.schema.json before exec, plus result.json after exec. This folder
exists so tests can snapshot argv construction without pointing Codex at
the repository root.
