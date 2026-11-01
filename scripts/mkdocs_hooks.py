"""MkDocs hooks for the documentation site (mkdocs.yml `hooks`).

GitHub renders `\\|` in a Markdown table cell as `|`, including inside a code span, so the
docs write a pipe in a table that way. Python-Markdown keeps the backslash inside code
spans, so the site would show `compile\\|run`. This removes it from code spans in table
cells only: anywhere else, a `\\|` (in a regex, say) is meant literally.
"""

import re

_CELL = re.compile(r"(<t[dh][^>]*>)(.*?)(</t[dh]>)", re.S)
_CODE = re.compile(r"(<code[^>]*>)(.*?)(</code>)", re.S)


def _unescape_code(cell):
    return _CODE.sub(lambda m: m.group(1) + m.group(2).replace("\\|", "|") + m.group(3), cell)


def on_page_content(html, **_kwargs):
    """Turns `\\|` back into `|` in the code spans of table cells."""
    return _CELL.sub(lambda m: m.group(1) + _unescape_code(m.group(2)) + m.group(3), html)
