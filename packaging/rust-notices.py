#!/usr/bin/env python3
"""Collect the license and notice files supplied by Cargo dependencies."""

import json
from pathlib import Path
import sys


def main():
    metadata = json.load(sys.stdin)
    dependencies = set(node["id"] for node in metadata["resolve"]["nodes"])
    dependencies.difference_update(metadata["workspace_members"])
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] not in dependencies:
            continue
        directory = Path(package["manifest_path"]).parent
        print(f"\n{package['name']} {package['version']} ({package['license'] or 'see license file'})")
        paths = {
            path
            for path in directory.iterdir()
            if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))
        }
        if package["license_file"]:
            paths.add(directory / package["license_file"])
        for path in sorted(paths):
            print(f"\n--- {path.name} ---\n{path.read_text(encoding='utf-8')}")


if __name__ == "__main__":
    main()
