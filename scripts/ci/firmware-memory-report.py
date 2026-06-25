#!/usr/bin/env python3
"""Report ESP32 firmware ELF RAM usage.

Usage:
    scripts/ci/firmware-memory-report.py [ELF]

Defaults to target/xtensa-esp32-none-elf/release/esp32-rust-project.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

DEFAULT_ELF = Path("target/xtensa-esp32-none-elf/release/esp32-rust-project")
DRAM_SEG_TOTAL = 192 * 1024
DRAM_SEG_END = 0x3FFE_0000
DRAM2_SEG_TOTAL = 98_768
RAM_SYMBOL_SECTIONS = (
    ".bss",
    ".data",
    ".dram2",
    ".noinit",
    ".rwdata",
)
KEY_SECTIONS = (".data", ".data.wifi", ".bss", ".dram2_uninit")


@dataclass(frozen=True)
class Section:
    name: str
    size: int
    vma: int


@dataclass(frozen=True)
class Symbol:
    name: str
    section: str
    size: int
    addr: int


def find_tool(name: str) -> str:
    candidates = [
        name,
        f"xtensa-esp32-elf-{name}",
        f"xtensa-esp-elf-{name}",
    ]
    for candidate in candidates:
        path = shutil.which(candidate)
        if path:
            return path
    raise SystemExit(
        f"Could not find {name!r} tool. Ensure the ESP Xtensa toolchain is on PATH."
    )


def run_tool(tool: str, *args: str) -> str:
    try:
        return subprocess.check_output([tool, *args], text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as exc:
        raise SystemExit(exc.output) from exc


def parse_sections(objdump_h: str) -> dict[str, Section]:
    sections: dict[str, Section] = {}
    for line in objdump_h.splitlines():
        fields = line.split()
        if len(fields) < 7:
            continue
        # Example:
        #  4 .bss 000191b4 3ffb45b8 3ffb45b8 0002b5b4 2**3
        if not fields[0].isdigit() or not fields[1].startswith("."):
            continue
        try:
            size = int(fields[2], 16)
            vma = int(fields[3], 16)
        except ValueError:
            continue
        sections[fields[1]] = Section(fields[1], size, vma)
    return sections


def parse_symbols(objdump_t: str) -> list[Symbol]:
    symbols: list[Symbol] = []
    for line in objdump_t.splitlines():
        fields = line.split()
        if len(fields) != 6:
            continue
        try:
            addr = int(fields[0], 16)
            size = int(fields[4], 16)
        except ValueError:
            continue
        section = fields[3]
        if size == 0 or not any(section.startswith(prefix) for prefix in RAM_SYMBOL_SECTIONS):
            continue
        symbols.append(Symbol(fields[5], section, size, addr))
    return symbols


def section_size(sections: dict[str, Section], name: str) -> int:
    return sections.get(name, Section(name, 0, 0)).size


def dram_static_end(sections: dict[str, Section]) -> int | None:
    dram_sections = [
        section
        for section in sections.values()
        if section.name in (".data", ".data.wifi", ".bss")
    ]
    if not dram_sections:
        return None
    return max(section.vma + section.size for section in dram_sections)


def print_bytes(label: str, value: int) -> None:
    print(f"{label:<32} {value:>8} bytes  ({value / 1024:>7.2f} KiB)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("elf", nargs="?", type=Path, default=DEFAULT_ELF)
    parser.add_argument("--top", type=int, default=25, help="number of RAM symbols to show")
    args = parser.parse_args()

    elf = args.elf
    if not elf.exists():
        print(f"ELF not found: {elf}", file=sys.stderr)
        print("Build firmware first, e.g. `cd firmware && cargo build --release`.", file=sys.stderr)
        return 2

    objdump = find_tool("objdump")
    sections = parse_sections(run_tool(objdump, "-h", str(elf)))
    symbols = parse_symbols(run_tool(objdump, "-t", str(elf)))

    print(f"ELF: {elf}")
    print()

    print("Largest RAM-resident symbols")
    print("----------------------------")
    for symbol in sorted(symbols, key=lambda item: item.size, reverse=True)[: args.top]:
        print(f"{symbol.size:8}  {symbol.section:<14}  {symbol.name}")
    print()

    print("Key section sizes")
    print("-----------------")
    for name in KEY_SECTIONS:
        print_bytes(name, section_size(sections, name))
    print()

    data_total = sum(section_size(sections, name) for name in (".data", ".data.wifi", ".bss"))
    end = dram_static_end(sections)
    if end is not None and end <= DRAM_SEG_END:
        pro_stack = DRAM_SEG_END - end
        pro_stack_note = "inferred from end of .data/.data.wifi/.bss"
    else:
        pro_stack = 75_924
        pro_stack_note = "fallback known current value"
    dram_used = data_total + pro_stack
    dram_free = DRAM_SEG_TOTAL - dram_used

    dram2_used = section_size(sections, ".dram2_uninit")
    dram2_free = DRAM2_SEG_TOTAL - dram2_used

    print("ESP32 DRAM headroom estimate")
    print("----------------------------")
    print_bytes("dram_seg total", DRAM_SEG_TOTAL)
    print_bytes("dram_seg statics", data_total)
    print_bytes(f"PRO CPU stack ({pro_stack_note})", pro_stack)
    print_bytes("dram_seg free", dram_free)
    print()
    print_bytes("dram2_seg total", DRAM2_SEG_TOTAL)
    print_bytes("dram2_seg .dram2_uninit", dram2_used)
    print_bytes("dram2_seg free", dram2_free)

    if dram_free < 0 or dram2_free < 0:
        print("\nWARNING: estimated RAM use exceeds known ESP32 segment size.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
