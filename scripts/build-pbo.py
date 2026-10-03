#!/usr/bin/env python3
"""Pack a directory into an uncompressed Bohemia PBO (DayZ/Arma format).

    scripts/build-pbo.py <source-dir> <output.pbo> --prefix <addon prefix>

Format: a "Vers" product entry carrying the prefix property, one header entry per
file (name\\0, packing, original size, reserved, timestamp, data size), a zero
terminator entry, the file payloads in order, then a zero byte and a SHA-1 of
everything before it. No tool dependency; mirrors what armake/mikero produce.
"""

from __future__ import annotations

import argparse
import hashlib
import struct
import sys
from pathlib import Path


def header_entry(
    name: bytes, packing: int, original: int, reserved: int, timestamp: int, size: int
) -> bytes:
    return (
        name
        + b"\0"
        + struct.pack("<IIIII", packing, original, reserved, timestamp, size)
    )


def build(source: Path, output: Path, prefix: str) -> int:
    files = sorted(
        p
        for p in source.rglob("*")
        if p.is_file() and p.name not in {"$PBOPREFIX$", "$PROPERTIES$"}
    )
    if not files:
        print(f"no files under {source}", file=sys.stderr)
        return 1
    body = bytearray()
    body += header_entry(b"", 0x56657273, 0, 0, 0, 0)
    body += b"prefix\0" + prefix.encode() + b"\0" + b"\0"
    payload = bytearray()
    for path in files:
        data = path.read_bytes()
        relative = path.relative_to(source).as_posix().replace("/", "\\").encode()
        body += header_entry(
            relative, 0, len(data), 0, int(path.stat().st_mtime), len(data)
        )
        payload += data
    body += header_entry(b"", 0, 0, 0, 0, 0)
    body += payload
    body += b"\0" + hashlib.sha1(bytes(body), usedforsecurity=False).digest()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(bytes(body))
    print(f"wrote {output} ({len(files)} files, {len(body)} bytes, prefix {prefix})")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--prefix", required=True)
    args = parser.parse_args(argv)
    return build(args.source, args.output, args.prefix)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
