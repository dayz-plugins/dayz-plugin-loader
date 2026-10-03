#!/usr/bin/env python3
"""Unpack a Bohemia PBO (DayZ/Arma format) into a directory.

    scripts/unpack-pbo.py <pbo> <outdir> [--list] [--verbose]

Format (mirrors scripts/build-pbo.py): header entries of ``name\\0`` followed by
five little-endian uint32 (packing method, original size, reserved, timestamp,
data size). The first entry is normally an empty-name "Vers" product entry
(packing 0x56657273) followed by null-terminated key/value string pairs ended by
an empty string. The header ends with an empty-name entry of 20 zero bytes, then
the payloads follow in header order. Entries with packing 0x43707273 ("Cprs")
are BI LZSS compressed and are expanded here; unknown methods are rejected.
Product properties (e.g. ``prefix``) are written to ``$PBOPREFIX$`` /
``$PROPERTIES$`` files in the output root.
"""

from __future__ import annotations

import argparse
import struct
import sys
from dataclasses import dataclass
from pathlib import Path

PACKING_VERS = 0x56657273  # "Vers"
PACKING_CPRS = 0x43707273  # "Cprs"
PACKING_RAW = 0
HEADER_STRUCT = struct.Struct("<IIIII")


@dataclass
class Entry:
    name: str
    packing: int
    original_size: int
    reserved: int
    timestamp: int
    data_size: int


class PboError(Exception):
    pass


def read_cstring(data: bytes, offset: int) -> tuple[bytes, int]:
    end = data.find(b"\0", offset)
    if end < 0:
        raise PboError("unterminated string in header")
    return data[offset:end], end + 1


def parse_header(data: bytes) -> tuple[dict[str, str], list[Entry], int]:
    """Return (properties, entries, payload_offset)."""
    offset = 0
    properties: dict[str, str] = {}
    entries: list[Entry] = []
    while True:
        raw_name, offset = read_cstring(data, offset)
        if offset + HEADER_STRUCT.size > len(data):
            raise PboError("truncated header entry")
        packing, original, reserved, timestamp, size = HEADER_STRUCT.unpack_from(
            data, offset
        )
        offset += HEADER_STRUCT.size
        if raw_name == b"" and packing == PACKING_VERS:
            while True:
                key, offset = read_cstring(data, offset)
                if key == b"":
                    break
                value, offset = read_cstring(data, offset)
                properties[key.decode("latin-1")] = value.decode("latin-1")
            continue
        if raw_name == b"":
            # Terminator: 20 zero bytes (some packers leave garbage; accept any).
            break
        entries.append(
            Entry(
                raw_name.decode("latin-1"), packing, original, reserved, timestamp, size
            )
        )
    return properties, entries, offset


def lzss_decompress(src: bytes, expected: int) -> bytes:
    """Bohemia LZSS: flag byte, 8 items; bit set = literal, clear = back-reference.

    Back-reference: two bytes b1, b2; rpos = i - ((b1 | ((b2 & 0xF0) << 4)) );
    length = (b2 & 0x0F) + 3. If rpos is before the start of the output the
    missing bytes are spaces (0x20). The stream is followed by a 4-byte
    checksum (sum of output bytes), which is required and validated.
    """
    if expected > 512 * 1024 * 1024:
        raise PboError("LZSS entry exceeds the 512 MiB output limit")
    out = bytearray()
    pos = 0
    n = len(src)
    while len(out) < expected:
        if pos >= n:
            raise PboError("LZSS stream ended early")
        flags = src[pos]
        pos += 1
        for bit in range(8):
            if len(out) >= expected:
                break
            if flags & (1 << bit):
                if pos >= n:
                    raise PboError("LZSS literal past end")
                out.append(src[pos])
                pos += 1
            else:
                if pos + 1 >= n:
                    raise PboError("LZSS pointer past end")
                b1, b2 = src[pos], src[pos + 1]
                pos += 2
                distance = b1 | ((b2 & 0xF0) << 4)
                if distance == 0:
                    raise PboError("LZSS back-reference has zero distance")
                rpos = len(out) - distance
                rlen = (b2 & 0x0F) + 3
                for _ in range(rlen):
                    if len(out) >= expected:
                        break
                    if rpos < 0:
                        out.append(0x20)
                    else:
                        out.append(out[rpos])
                    rpos += 1
    if pos + 4 != n:
        raise PboError("LZSS checksum is missing or has trailing data")
    (checksum,) = struct.unpack_from("<I", src, pos)
    if checksum != sum(out) & 0xFFFFFFFF:
        raise PboError("LZSS checksum mismatch")
    return bytes(out)


def safe_target(outdir: Path, name: str) -> Path:
    normalized = name.replace("\\", "/")
    parts = normalized.split("/")
    if any(part in ("", ".", "..") or ":" in part for part in parts):
        raise PboError(f"unsafe entry name {name!r}")
    root = outdir.resolve()
    target = root
    for part in parts:
        target = target / part
        if target.is_symlink():
            raise PboError(f"entry destination contains a symlink: {name!r}")
    if not target.resolve().is_relative_to(root):
        raise PboError(f"entry escapes output directory: {name!r}")
    return target


def validate_targets(
    outdir: Path, entries: list[Entry], properties: dict[str, str]
) -> None:
    # Validate the entire namespace before creating any files. Case-insensitive
    # collisions matter because these archives are normally consumed on Windows.
    names = [entry.name for entry in entries]
    if "prefix" in properties:
        names.append("$PBOPREFIX$")
    if properties:
        names.append("$PROPERTIES$")
    seen: set[str] = set()
    for name in names:
        safe_target(outdir, name)
        normalized = name.replace("\\", "/").casefold()
        if normalized in seen:
            raise PboError(f"duplicate entry destination: {name!r}")
        seen.add(normalized)
    for name in names:
        components = name.replace("\\", "/").casefold().split("/")
        if any(
            "/".join(components[:index]) in seen for index in range(1, len(components))
        ):
            raise PboError(f"file/directory collision: {name!r}")


def unpack(pbo: Path, outdir: Path, list_only: bool, verbose: bool) -> int:
    data = pbo.read_bytes()
    properties, entries, offset = parse_header(data)
    if list_only:
        for key, value in properties.items():
            print(f"property {key}={value}")
        for e in entries:
            method = {PACKING_RAW: "raw", PACKING_CPRS: "lzss"}.get(
                e.packing, f"0x{e.packing:08x}"
            )
            print(f"{e.data_size:>10} {e.original_size:>10} {method:<10} {e.name}")
        print(f"{len(entries)} entries, payload at {offset}, file {len(data)} bytes")
        return 0

    validate_targets(outdir, entries, properties)
    payloads: list[tuple[Entry, bytes]] = []
    compressed = 0
    total_size = 0
    for entry in entries:
        total_size += (
            entry.original_size if entry.packing == PACKING_CPRS else entry.data_size
        )
        if total_size > 2 * 1024 * 1024 * 1024:
            raise PboError("archive exceeds the 2 GiB output limit")
        blob = data[offset : offset + entry.data_size]
        offset += entry.data_size
        if len(blob) != entry.data_size:
            raise PboError(f"truncated payload for {entry.name}")
        if entry.packing == PACKING_CPRS:
            compressed += 1
            try:
                blob = lzss_decompress(blob, entry.original_size)
            except PboError as error:
                raise PboError(f"{entry.name}: {error}") from error
        elif entry.packing != PACKING_RAW:
            raise PboError(f"{entry.name}: unknown packing 0x{entry.packing:08x}")
        payloads.append((entry, blob))

    # No malformed entry can leave compressed/raw bytes masquerading as a valid
    # extracted source file. Publish only after every payload has been decoded.
    outdir.mkdir(parents=True, exist_ok=True)
    if "prefix" in properties:
        safe_target(outdir, "$PBOPREFIX$").write_text(
            properties["prefix"] + "\n", encoding="latin-1"
        )
    if properties:
        lines = "".join(f"{key}={value}\n" for key, value in properties.items())
        safe_target(outdir, "$PROPERTIES$").write_text(lines, encoding="latin-1")
    for entry, blob in payloads:
        target = safe_target(outdir, entry.name)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(blob)
        if verbose:
            print(target)
    written = len(payloads)

    remaining = len(data) - offset
    print(
        f"{pbo.name}: {written} files -> {outdir} "
        f"(compressed {compressed}, "
        f"trailing {remaining} bytes, prefix {properties.get('prefix', '-')})"
    )
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("pbo", type=Path)
    parser.add_argument("outdir", type=Path)
    parser.add_argument(
        "--list", action="store_true", help="list entries instead of extracting"
    )
    parser.add_argument(
        "--verbose", action="store_true", help="print each written path"
    )
    args = parser.parse_args(argv)
    try:
        return unpack(args.pbo, args.outdir, args.list, args.verbose)
    except (PboError, OSError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
