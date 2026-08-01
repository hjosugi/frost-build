#!/usr/bin/env python3
"""Validate the dependency-free GitHub Pages site."""

from __future__ import annotations

from html.parser import HTMLParser
from pathlib import Path
import re
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1] / "site"
DOCS_START = "/* Documentation hub */"
DOCS_END = "/* End documentation components */"


class Document(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.ids: set[str] = set()
        self.references: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        del tag
        attributes = dict(attrs)
        if element_id := attributes.get("id"):
            self.ids.add(element_id)
        for key in ("href", "src"):
            if reference := attributes.get(key):
                self.references.append(reference)


def validate_css() -> None:
    css = (ROOT / "styles.css").read_text(encoding="utf-8")
    structural = re.sub(r"/\*.*?\*/", "", css, flags=re.DOTALL)
    structural = re.sub(r"""(["']).*?(?<!\\)\1""", "", structural, flags=re.DOTALL)
    assert structural.count("{") == structural.count("}"), "unbalanced CSS braces"

    definitions = set(re.findall(r"(?m)^\s*(--[\w-]+)\s*:", css))
    uses = set(re.findall(r"var\(\s*(--[\w-]+)", css))
    assert not (undefined := sorted(uses - definitions)), (
        f"undefined CSS custom properties: {undefined}"
    )

    docs_css = css.split(DOCS_START, 1)[1].split(DOCS_END, 1)[0]
    for property_name in ("font-size", "line-height"):
        values = [
            value.strip()
            for value in re.findall(rf"{property_name}:\s*([^;]+)", docs_css)
        ]
        literals = [value for value in values if not value.startswith("var(")]
        assert not literals, (
            f"documentation {property_name} must use a token: {literals}"
        )

    print(
        f"CSS: {len(definitions)} custom properties; "
        "references defined; Docs typography tokenized"
    )


def validate_html() -> None:
    pages: dict[Path, Document] = {}
    for html in sorted(ROOT.rglob("*.html")):
        document = Document()
        document.feed(html.read_text(encoding="utf-8"))
        pages[html.resolve()] = document

    for html, document in pages.items():
        for reference in document.references:
            parsed = urlsplit(reference)
            if parsed.scheme or reference.startswith("//"):
                continue
            target = html if not parsed.path else (html.parent / parsed.path).resolve()
            if target.is_dir():
                target /= "index.html"
            assert target.exists(), f"{html}: missing local resource {reference}"
            if parsed.fragment:
                assert target in pages, (
                    f"{html}: anchor target is not HTML: {reference}"
                )
                assert parsed.fragment in pages[target].ids, (
                    f"{html}: missing anchor {reference}"
                )

    print(f"HTML: {len(pages)} files; local resources and anchors resolve")


def main() -> None:
    validate_css()
    validate_html()


if __name__ == "__main__":
    main()
