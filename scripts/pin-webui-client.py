#!/usr/bin/env python3
"""Re-pins the webui client's manifest rows after `just client-check`
re-emits and gzips the bundle.

Rewrites the marker-delimited client section of the vendor manifest
with a fresh sha256 per emitted `.js.gz` file. The markers are
load-bearing: build.rs parses the whole `[[assets]]` array, and rows
outside the markers (third-party dists) are hand-vendored and must
never be touched by this script.

Usage: pin-webui-client.py <vendor-manifest.toml> <vendor-dir>
"""

import hashlib
import os
import sys

BEGIN = "# BEGIN client rows (managed by just client-check)"
END = "# END client rows"


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <vendor-manifest.toml> <vendor-dir>")
    manifest_path, vendor_dir = sys.argv[1], sys.argv[2]

    text = open(manifest_path, encoding="utf-8").read()
    if BEGIN not in text or END not in text:
        sys.exit(f"manifest missing the {BEGIN!r} / {END!r} markers")
    head, _, rest = text.partition(BEGIN)
    _, _, tail = rest.partition(END)

    gzipped = sorted(f[:-3] for f in os.listdir(vendor_dir) if f.endswith(".js.gz"))
    rows = [BEGIN, ""]
    for name in gzipped:
        data = open(os.path.join(vendor_dir, name + ".gz"), "rb").read()
        digest = hashlib.sha256(data).hexdigest()
        rows += [
            "[[assets]]",
            f'served = "{name}"',
            f'sha256 = "{digest}"',
            'encoding = "gzip"',
            "",
        ]
    open(manifest_path, "w", encoding="utf-8").write(
        head + "\n".join(rows) + END + tail
    )
    print(f"    pinned {len(gzipped)} client module(s)")


if __name__ == "__main__":
    main()
