"""A Python-Markdown extension for the documentation site (zensical.toml).

GitHub renders `\\|` in a Markdown table cell as `|`, including inside a code span, so the
docs write a pipe in a table that way. Python-Markdown keeps the backslash inside code
spans, so the site would show `compile\\|run`. This removes it from code spans in table
cells only: anywhere else, a `\\|` (in a regex, say) is meant literally.

Zensical renders with Python-Markdown and loads this by its module name,
`scripts.ods_markdown`, so build the site from the repository root with
`python -m zensical`, which puts the current directory on the import path.
"""

import re

from markdown import Extension
from markdown.postprocessors import Postprocessor

_CELL = re.compile(r"(<t[dh][^>]*>)(.*?)(</t[dh]>)", re.S)
_CODE = re.compile(r"(<code[^>]*>)(.*?)(</code>)", re.S)


def _unescape_code(cell):
    return _CODE.sub(lambda m: m.group(1) + m.group(2).replace("\\|", "|") + m.group(3), cell)


def unescape_table_pipes(html):
    """Turns `\\|` back into `|` in the code spans of table cells."""
    return _CELL.sub(lambda m: m.group(1) + _unescape_code(m.group(2)) + m.group(3), html)


class _TablePipes(Postprocessor):
    def run(self, text):
        return unescape_table_pipes(text)


class OdsMarkdown(Extension):
    """The site's Markdown fixes; see the module docs."""

    def extendMarkdown(self, md):  # noqa: N802 (Python-Markdown's name)
        # After the raw-HTML placeholders are restored, so it sees the final cells.
        md.postprocessors.register(_TablePipes(md), "ods_table_pipes", 0)


def makeExtension(**kwargs):  # noqa: N802 (Python-Markdown's name)
    return OdsMarkdown(**kwargs)


if __name__ == "__main__":
    # A self-test, as the other scripts in scripts/ have.
    cell = "<td><code>compile\\|run</code> a\\|b</td>"
    assert unescape_table_pipes(cell) == "<td><code>compile|run</code> a\\|b</td>", cell
    assert unescape_table_pipes("<p><code>a\\|b</code></p>") == "<p><code>a\\|b</code></p>"
    print("ods_markdown self-test: ok")
