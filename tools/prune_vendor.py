"""
Prune vendor/bin down to the DLLs libtesseract-5.dll actually needs.

The Tesseract installer ships ~50 DLLs, most of them only used by its training
and rendering utilities (cairo, pango, archive, ...). We walk the PE import
tables transitively from libtesseract-5.dll and move anything unreachable to a
holding directory, so a mistake is a move back rather than a re-download.

    python tools/prune_vendor.py            # report only
    python tools/prune_vendor.py --apply    # move unreachable DLLs aside
"""
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "vendor" / "bin"
ATTIC = ROOT / "vendor" / "_unused"
ROOTS = ["libtesseract-5.dll"]


def _rva_to_offset(rva, sections):
    for va, vsize, raw_size, raw_ptr in sections:
        if va <= rva < va + max(vsize, raw_size):
            return raw_ptr + (rva - va)
    return None


def imported_dlls(path: Path) -> set[str]:
    """DLL names from the import and delay-import directories of a PE file."""
    data = path.read_bytes()
    if data[:2] != b"MZ":
        return set()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        return set()

    coff = pe + 4
    n_sections = struct.unpack_from("<H", data, coff + 2)[0]
    opt_size = struct.unpack_from("<H", data, coff + 16)[0]
    opt = coff + 20
    magic = struct.unpack_from("<H", data, opt)[0]
    # Data directories sit after the optional header's fixed part, which differs
    # between PE32 (0x10b) and PE32+ (0x20b).
    dirs = opt + (112 if magic == 0x20B else 96)

    sections = []
    sec_off = opt + opt_size
    for i in range(n_sections):
        s = sec_off + i * 40
        va, raw_size, raw_ptr = struct.unpack_from("<III", data, s + 12)
        vsize = struct.unpack_from("<I", data, s + 8)[0]
        sections.append((va, vsize, raw_size, raw_ptr))

    names = set()
    for index, descriptor_size, name_field in ((1, 20, 12), (13, 32, 4)):
        try:
            rva, size = struct.unpack_from("<II", data, dirs + index * 8)
        except struct.error:
            continue
        if not rva or not size:
            continue
        off = _rva_to_offset(rva, sections)
        if off is None:
            continue
        while True:
            chunk = data[off : off + descriptor_size]
            if len(chunk) < descriptor_size or not any(chunk):
                break
            name_rva = struct.unpack_from("<I", data, off + name_field)[0]
            if name_rva:
                n_off = _rva_to_offset(name_rva, sections)
                if n_off is not None:
                    end = data.index(b"\0", n_off)
                    names.add(data[n_off:end].decode("ascii", "replace").lower())
            off += descriptor_size
    return names


def main() -> None:
    apply = "--apply" in sys.argv
    present = {p.name.lower(): p for p in BIN.glob("*.dll")}

    needed, queue = set(), list(ROOTS)
    while queue:
        name = queue.pop().lower()
        if name in needed or name not in present:
            continue
        needed.add(name)
        queue.extend(imported_dlls(present[name]))

    unused = sorted(set(present) - needed)
    keep_size = sum(present[n].stat().st_size for n in needed)
    drop_size = sum(present[n].stat().st_size for n in unused)

    print(f"keep   {len(needed):>3} dll  {keep_size / 1e6:>7.1f} MB")
    for n in sorted(needed):
        print(f"       {n}")
    print(f"unused {len(unused):>3} dll  {drop_size / 1e6:>7.1f} MB")
    print("       " + ", ".join(unused))

    if not apply:
        print("\n(dry run; pass --apply to move the unused ones to vendor/_unused)")
        return

    ATTIC.mkdir(parents=True, exist_ok=True)
    for n in unused:
        present[n].rename(ATTIC / present[n].name)
    print(f"\nmoved {len(unused)} dll to {ATTIC}")


if __name__ == "__main__":
    main()
