"""Keep agent instructions readable and current reference links usable."""

from __future__ import annotations

from html.parser import HTMLParser
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit

import pytest


ROOT = Path(__file__).resolve().parents[2]
DOCUMENTS = [ROOT / name for name in ("AGENTS.md", "README.md", "README.zh-CN.md")]
DOCUMENTS += sorted((ROOT / "docs/reference").rglob("*.md"))
TRANSLATIONS = sorted((ROOT / "docs/reference").rglob("*.zh-CN.md"))


def _without_code(markdown: str) -> str:
    lines = []
    fence = None
    for line in markdown.splitlines():
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line)
        if fence is not None:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence) and not marker[2].strip():
                fence = None
            lines.append("")
        elif marker:
            fence = marker[1]
            lines.append("")
        elif line.startswith(("    ", "\t")):
            lines.append("")
        else:
            lines.append(line)
    text = "\n".join(lines)
    return re.sub(r"(`+).*?(?<!`)\1(?!`)", "", text, flags=re.DOTALL)


def _destinations(markdown: str):
    text = _without_code(markdown)
    # Reference definitions cover full, collapsed and shortcut reference links.
    starts = [match.end() for match in re.finditer(r"\]\(\s*", text)]
    starts += [match.end() for match in re.finditer(r"^ {0,3}\[[^\]\n]+\]:\s*", text, re.MULTILINE)]
    for start in starts:
        if start >= len(text):
            continue
        if text[start] == "<":
            end = text.find(">", start + 1)
            if end >= 0:
                yield text[start + 1:end]
            continue
        end = start
        depth = 0
        while end < len(text):
            char = text[end]
            if char == "\\" and end + 1 < len(text):
                end += 2
                continue
            if char.isspace() or (char == ")" and depth == 0):
                break
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
            end += 1
        if end > start:
            yield re.sub(r"\\([\\() ])", r"\1", text[start:end])
    # READMEs also use HTML for banners, badges and language navigation.
    parser = _HtmlLinks()
    parser.feed(text)
    yield from parser.destinations


class _HtmlLinks(HTMLParser):
    def __init__(self):
        super().__init__()
        self.destinations: list[str] = []

    def handle_starttag(self, tag, attrs):
        self.destinations.extend(
            value for name, value in attrs if name in {"href", "src"} and value
        )


def test_link_scan_covers_markdown_and_html_and_excludes_code():
    document = """
```markdown
[ignored](missing-fenced.md)
```
~~~html
<a href="missing-tilde.md">ignored</a>
~~~
    [ignored](missing-indented.md)
`[ignored](missing-inline.md)`
``<a href="missing-inline-html.md">ignored</a>``
[direct](guide.md#details)
![spaced](<images/with space.png>)
[balanced](guides/example(draft).md)
[reference][guide]
[guide]: reference.md "Guide title"
<a href="html-guide.md"><img src="images/banner.png"></a>
"""
    assert set(_destinations(document)) == {
        "guide.md#details", "images/with space.png", "guides/example(draft).md",
        "reference.md", "html-guide.md", "images/banner.png",
    }


def test_root_agent_instructions_fit_loader_budget():
    size = (ROOT / "AGENTS.md").stat().st_size
    assert size <= 32_768, f"AGENTS.md is {size:,} bytes; move mechanism detail into docs/reference/"


@pytest.mark.parametrize("document", DOCUMENTS, ids=lambda path: path.relative_to(ROOT).as_posix())
def test_relative_documentation_links_resolve(document):
    missing = []
    for destination in _destinations(document.read_text(encoding="utf-8")):
        url = urlsplit(destination)
        if url.scheme or url.netloc or not url.path or url.path.startswith("/"):
            continue
        path = document.parent / unquote(url.path)
        if not path.exists():
            missing.append(destination)
    assert not missing, f"{document.relative_to(ROOT)} has missing link targets: {missing}"


@pytest.mark.parametrize("translation", TRANSLATIONS, ids=lambda path: path.relative_to(ROOT).as_posix())
def test_reference_language_pairs_exist_and_link_to_each_other(translation):
    english = translation.with_name(translation.name.removesuffix(".zh-CN.md") + ".md")
    assert english.is_file(), f"{translation.relative_to(ROOT)} has no English counterpart"
    for document, counterpart in ((english, translation), (translation, english)):
        targets = set()
        for destination in _destinations(document.read_text(encoding="utf-8")):
            url = urlsplit(destination)
            if not url.scheme and not url.netloc and url.path:
                targets.add((document.parent / unquote(url.path)).resolve())
        assert counterpart.resolve() in targets, (
            f"{document.relative_to(ROOT)} must link to {counterpart.name}"
        )
