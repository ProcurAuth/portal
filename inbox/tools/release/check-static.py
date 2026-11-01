#!/usr/bin/env python3
"""Fails unless every given ELF file is statically linked (no PT_INTERP, no DT_NEEDED)."""
import struct
import sys

PT_INTERP, PT_DYNAMIC, DT_NEEDED = 3, 2, 1


def check(path: str) -> list[str]:
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"\x7fELF":
        return [f"{path}: not an ELF file"]
    is64 = data[4] == 2
    end = "<" if data[5] == 1 else ">"
    if is64:
        phoff, = struct.unpack_from(end + "Q", data, 0x20)
        phentsize, phnum = struct.unpack_from(end + "HH", data, 0x36)
    else:
        phoff, = struct.unpack_from(end + "I", data, 0x1C)
        phentsize, phnum = struct.unpack_from(end + "HH", data, 0x2A)
    problems = []
    for i in range(phnum):
        off = phoff + i * phentsize
        p_type, = struct.unpack_from(end + "I", data, off)
        if p_type == PT_INTERP:
            problems.append(f"{path}: has a program interpreter (dynamically linked)")
        if p_type == PT_DYNAMIC:
            if is64:
                p_offset, = struct.unpack_from(end + "Q", data, off + 8)
                p_filesz, = struct.unpack_from(end + "Q", data, off + 32)
                fmt, size = end + "qQ", 16
            else:
                p_offset, = struct.unpack_from(end + "I", data, off + 4)
                p_filesz, = struct.unpack_from(end + "I", data, off + 16)
                fmt, size = end + "iI", 8
            for j in range(p_offset, p_offset + p_filesz, size):
                tag, _ = struct.unpack_from(fmt, data, j)
                if tag == 0:
                    break
                if tag == DT_NEEDED:
                    problems.append(f"{path}: needs a shared library")
                    break
    return problems


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: check-static.py <elf>...", file=sys.stderr)
        return 2
    problems = [p for path in sys.argv[1:] for p in check(path)]
    for p in problems:
        print(p, file=sys.stderr)
    if not problems:
        print("static: " + " ".join(sys.argv[1:]))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
