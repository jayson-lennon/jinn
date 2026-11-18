#!/usr/bin/env python3
"""Re-pins the webui client's manifest rows after `just webui-build`
re-emits and gzips the bundle.

Rewrites the marker-delimited client section of the vendor manifest
with a fresh sha256 per emitted `.js.gz` file. The markers are
load-bearing: build.rs parses the whole `[[assets]]` array, and rows
outside the markers (third-party dists) are hand-vendored and must
never be touched by this script.

Usage: pin-webui-client.py <vendor-manifest.toml> <vendor-dir>
"""

import gzip
import hashlib
import os
import re
import sys

BEGIN = "# BEGIN client rows (managed by just webui-build)"
END = "# END client rows"

# Import-specifier rewrite: the emitted modules import each other by
# bare relative URL ("./api.js", "../wire.js"), which would bypass the
# cache-busting URLs. Each specifier gains the target's content hash so
# every shipped URL is immutable. Runs on the gzipped emit (gunzip,
# rewrite, re-gzip deterministically), BEFORE the manifest pin — the
# pinned sha256 names the shipped bytes, rewritten imports included.
#
# Two passes: pass 1 rewrites bare specifiers with provisional hashes
# and re-gzips (changing every importing file's content), pass 2
# recomputes the final per-file hashes and substitutes them in — a
# file's own hash never appears in its own imports, so pass 2's
# substitution cannot invalidate pass 2's other hashes.
IMPORT_RE = re.compile(r'(from\s*")(\.{1,2}/[\w/]+)\.js(")')


def specifier_to_served(specifier: str, module: str) -> str:
    """Resolves one emitted import specifier against its importing
    module's vendor-relative directory, yielding the served path."""
    base = os.path.dirname(module)
    return os.path.normpath(os.path.join(base, specifier)).replace(os.sep, "/")


def gunzip(gz_path: str) -> str:
    return gzip.open(gz_path, "rt", encoding="utf-8").read()


def regzip(gz_path: str, text: str) -> None:
    with open(gz_path, "wb") as fh:
        fh.write(gzip.compress(text.encode("utf-8"), mtime=0))


def parse_imports(text: str, names: list[str], module: str) -> list[str]:
    """Returns the manifest names a module's bare imports reference."""
    targets = []
    for candidate in IMPORT_RE.finditer(text):
        target = specifier_to_served(candidate.group(2), module) + ".js"
        if target not in names:
            sys.exit(
                f"a module imports ./{candidate.group(2)}.js but no manifest row "
                f"vouches for it — the import graph and the vendor table disagree"
            )
        targets.append(target)
    return targets


def rewrite_imports(vendor_dir: str, names: list[str]) -> dict[str, str]:
    """Rewrites every import to a hash-bearing URL; returns final hashes.

    Modules are processed leaves-first (topological order): a module's
    imports are finalized before its own content is hashed, so the URL
    hash segment always equals the pinned sha256 prefix of the shipped
    bytes. Acyclic import graphs only — a cycle fails loudly with the
    leftover-work assertion.
    """
    pending = {
        name: parse_imports(
            gunzip(os.path.join(vendor_dir, name + ".gz")), names, name
        )
        for name in names
    }
    hashes: dict[str, str] = {}
    while pending:
        ready = [name for name, imports in pending.items()
                 if all(target in hashes for target in imports)]
        if not ready:
            sys.exit(
                "the client import graph has a cycle among: "
                f"{sorted(pending)} — hash stamping cannot converge"
            )
        for name in ready:
            del pending[name]
            gz_path = os.path.join(vendor_dir, name + ".gz")

            def sub(match: re.Match[str]) -> str:
                specifier = match.group(2)
                target = specifier_to_served(specifier, name) + ".js"
                return (
                    f'{match.group(1)}{specifier}.{hashes[target]}.js'
                    f'{match.group(3)}'
                )

            regzip(gz_path, IMPORT_RE.sub(sub, gunzip(gz_path)))
            hashes[name] = hashlib.sha256(
                open(gz_path, "rb").read()
            ).hexdigest()[:8]
    return hashes


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <vendor-manifest.toml> <vendor-dir>")
    manifest_path, vendor_dir = sys.argv[1], sys.argv[2]

    text = open(manifest_path, encoding="utf-8").read()
    if BEGIN not in text or END not in text:
        sys.exit(f"manifest missing the {BEGIN!r} / {END!r} markers")
    head, _, rest = text.partition(BEGIN)
    _, _, tail = rest.partition(END)

    # Walked, not enumerated: emitted modules may live in
    # subdirectories (the client's regions/), and a new module must
    # not require editing this script. `served` names the path under
    # the vendor mount, forward slashes, matching resolve_request.
    gzipped = sorted(
        os.path.relpath(os.path.join(root, f), vendor_dir).replace(os.sep, "/")[:-3]
        for root, _, files in os.walk(vendor_dir)
        for f in files
        if f.endswith(".js.gz")
    )

    # Rewrite the import graph to hash-bearing URLs, THEN pin — the
    # manifest's sha256 must name the shipped bytes, rewritten imports
    # included, and the URL hash segment must equal the pinned prefix.
    hashes = rewrite_imports(vendor_dir, gzipped)

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
