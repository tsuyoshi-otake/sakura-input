#!/usr/bin/env python3
"""Streams plain-text paragraphs out of a Japanese Wikipedia multistream part (#474).

The association-table prototype counts word co-occurrences in public CC BY-SA
4.0 text. Extraction stays in the Python standard library (`python -I`); the
counting itself lives in Rust (`assoc-table-build`). One
`jawiki-*-pages-articles-multistream*.bz2` part goes in on the command line,
paragraphs come out on stdout -- one paragraph per line, articles separated
by a form-feed-only line so the counter can keep its window inside articles.
Nothing is written to disk by this script; the caller deletes each part after
the pipeline drains.

Markup handling is deliberately crude, not a parser: templates and refs are
dropped, [[a|b]] -> b and [[a]] -> a, residual wiki syntax characters are
removed, and headings keep their title text. This is sufficient because the
counter only needs words in surface order inside a paragraph.

Usage:
    python -I scripts/extract-wiki-text.py <part.xml.bz2>
"""
import bz2
import re
import sys
import xml.etree.ElementTree as etree

NS = "{http://www.mediawiki.org/xml/export-0.11/}"

# Pages that are not article prose.
SKIP_PREFIXES = (
    "ファイル:",
    "File:",
    "Category:",
    "カテゴリ:",
    "Template:",
    "テンプレート:",
    "Help:",
    "ヘルプ:",
    "Portal:",
    "ポータル:",
    "Wikipedia:",
    "プロジェクト:",
    "Module:",
    "モジュール:",
    "MediaWiki:",
    "メディアウィキ:",
    "利用者:",
    "User:",
    "ノート:",
    "Talk:",
)

TEMPLATE_RE = re.compile(r"\{\{[^{}]*\}\}")
TAG_RE = re.compile(r"<[^>]*>")
LINK_RE = re.compile(r"\[\[([^\]|]*)\|?([^\]]*)\]\]")
EXTLINK_RE = re.compile(r"\[https?://[^\s\]]*\s?([^\]]*)\]")
TABLE_ROW_RE = re.compile(r"^\s*[\|!{].*$")
HEADING_RE = re.compile(r"^(=+)\s*(.*?)\s*\1\s*$")
LIST_RE = re.compile(r"^\s*[*#:;]+\s*")
RESIDUAL_RE = re.compile(r"['\[\]{}<>|]")


def clean(text):
    """Reduces a wikitext article to one plain paragraph per output line."""
    out = []
    # Repeatedly peel innermost templates; nested leftovers are dropped.
    previous = None
    while previous != text:
        previous = text
        text = TEMPLATE_RE.sub(" ", text)
    text = TAG_RE.sub(" ", text)
    for line in text.split("\n"):
        line = line.strip()
        if not line:
            if out:
                yield "\n".join(out)
                out = []
            continue
        if TABLE_ROW_RE.match(line):
            continue
        heading = HEADING_RE.match(line)
        if heading:
            line = heading.group(2)
        line = LIST_RE.sub("", line)
        # Rewrites keep the display text: [[a|b]] -> b, [[a]] -> a.
        line = LINK_RE.sub(lambda m: m.group(2) or m.group(1), line)
        line = EXTLINK_RE.sub(lambda m: m.group(1), line)
        line = RESIDUAL_RE.sub("", line).strip()
        if line:
            out.append(line)
    if out:
        yield "\n".join(out)


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: extract-wiki-text.py <part.xml.bz2>")
    path = sys.argv[1]
    write = sys.stdout.buffer.write
    with bz2.open(path, "rb") as source:
        context = etree.iterparse(source, events=("end",))
        for _, element in context:
            if element.tag != NS + "page":
                continue
            title = element.findtext(NS + "title", "")
            if title.startswith(SKIP_PREFIXES) or ":" in title.split("（")[0]:
                element.clear()
                continue
            revision = element.find(NS + "revision")
            text = revision.findtext(NS + "text", "") if revision is not None else ""
            for paragraph in clean(text):
                write(paragraph.encode("utf-8") + b"\n")
            write(b"\x0c\n")  # article boundary
            element.clear()


if __name__ == "__main__":
    main()
