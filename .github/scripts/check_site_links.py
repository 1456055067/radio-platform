"""Fail if any internal link or anchor in a built Zensical site is broken.

Usage: python -I .github/scripts/check_site_links.py site
"""

import os
import re
import sys
from urllib.parse import unquote, urlparse

HREF = re.compile(r'href="([^"]+)"')


def main(root: str) -> int:
    pages: dict[str, str] = {}
    broken: set[tuple[str, str]] = set()
    for directory, _, files in os.walk(root):
        for name in files:
            # The 404 page uses absolute links resolved against site_url.
            if not name.endswith(".html") or name == "404.html":
                continue
            path = os.path.join(directory, name)
            with open(path, encoding="utf-8") as f:
                html = f.read()
            for href in HREF.findall(html):
                url = urlparse(href)
                if url.scheme or href.startswith(("#", "mailto:")):
                    continue
                target = os.path.normpath(os.path.join(directory, unquote(url.path)))
                if os.path.isdir(target):
                    target = os.path.join(target, "index.html")
                if not os.path.exists(target):
                    broken.add((path, href))
                    continue
                if url.fragment and target.endswith(".html"):
                    if target not in pages:
                        with open(target, encoding="utf-8") as f:
                            pages[target] = f.read()
                    if f'id="{url.fragment}"' not in pages[target]:
                        broken.add((path, href))
    for path, href in sorted(broken):
        print(f"{path}: broken link {href}")
    print(f"{len(broken)} broken internal link(s)")
    return 1 if broken else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
