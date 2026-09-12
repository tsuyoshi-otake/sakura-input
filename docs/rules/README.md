# Verified rule topics

`.claude/memory/rules.md` is the unconditional entry point. Its trigger table
makes each matching topic here mandatory. Topic files are exact extracts from
the pre-compaction rules, including evidence and applicability details.

The byte-exact original is retained at `../history/rules.pre-154.md`. Add a new
rule to the narrowest topic, state what was measured, and update
`section-map.md`. Promote a rule to the unconditional entry point only when it
applies to nearly every repository task or is a machine-wide operating
constraint.
