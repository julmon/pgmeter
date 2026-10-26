#!/usr/bin/env python3
"""Check that every relative link and image in a built mdBook resolves.

Usage: check-links.py <book directory>

Walks the HTML files, collects the href and src attributes that are neither
absolute URLs nor fragments only, resolves them against the file's directory
and reports the ones that point to nothing. Fragments are checked against the
ids of the target page. Exit status 1 when anything is broken.
"""

import html.parser
import os
import sys
import urllib.parse


class Collector(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.ids = set()

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "a" and "name" in attrs:
            self.ids.add(attrs["name"])
        target = attrs.get("href") if tag in ("a", "link") else attrs.get("src")
        if target:
            self.links.append(target)


def parse(path):
    collector = Collector()
    with open(path, encoding="utf-8", errors="replace") as f:
        collector.feed(f.read())
    return collector


def main(root):
    pages = {}
    for dirpath, _, files in os.walk(root):
        for name in files:
            if name.endswith(".html"):
                path = os.path.join(dirpath, name)
                pages[path] = parse(path)

    broken = []
    for path, page in pages.items():
        base = os.path.dirname(path)
        for raw in page.links:
            url = urllib.parse.urlsplit(raw)
            if url.scheme or url.netloc or raw.startswith("mailto:"):
                continue
            if not url.path:
                if url.fragment and url.fragment not in page.ids:
                    broken.append((path, raw, "no such anchor"))
                continue
            target = os.path.normpath(os.path.join(base, urllib.parse.unquote(url.path)))
            if os.path.isdir(target):
                target = os.path.join(target, "index.html")
            if not os.path.isfile(target):
                broken.append((path, raw, "no such file"))
                continue
            if url.fragment and target.endswith(".html"):
                ids = pages.get(target, parse(target)).ids
                if url.fragment not in ids:
                    broken.append((path, raw, "no such anchor"))

    for path, raw, why in sorted(broken):
        print(f"{os.path.relpath(path, root)}: {raw}: {why}")
    print(f"{len(pages)} pages, {len(broken)} broken links")
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "docs/book"))
