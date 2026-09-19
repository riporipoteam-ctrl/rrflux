#!/usr/bin/env python3
"""
Builds a minimal x64 Windows DLL that stubs out EasyAntiCheat.

The stub exports the 11 Cerberus_* functions the Rec Room client P/Invokes
as no-ops (xor eax,eax; ret). With this in place of
RecRoom_Data/Plugins/x86_64/EasyAntiCheat.dll, the client's
EasyAntiCheat.Runtime.Initialize() resolves its imports and runs against
dead stubs instead of the real EAC backend (which is gone anyway).

Pure-Python PE writer — no Windows toolchain required.
"""

import struct

EXPORTS = [
    "Cerberus_BeginFrame",
    "Cerberus_EndFrame",
    "Cerberus_GameRoundEnd",
    "Cerberus_GameRoundStart",
    "Cerberus_PlayerDespawn",
    "Cerberus_PlayerDowned",
    "Cerberus_PlayerRevive",
    "Cerberus_PlayerSpawn",
    "Cerberus_PlayerTakeDamage",
    "Cerberus_PlayerTick",
    "Cerberus_PlayerUseWeapon",
]

DLL_NAME = "EasyAntiCheat.dll"
# xor eax,eax ; ret  — safe no-op on x64 (caller cleans the stack)
STUB = b"\x31\xc0\xc3"


def build_stub_dll() -> bytes:
    n = len(EXPORTS)
    # ---- .text: n stubs
    text = STUB * n
    # ---- .rdata: export directory
    # layout inside .rdata (file offset relative to section start):
    #   0x00: export directory (40 bytes)
    #   0x28: AddressOfFunctions (n*4)
    #   ...:  AddressOfNames (n*4)
    #   ...:  AddressOfNameOrdinals (n*2)
    #   ...:  name strings, then DLL name
    dir_size = 40
    func_tab = dir_size
    name_tab = func_tab + n * 4
    ord_tab = name_tab + n * 4
    str_off = ord_tab + n * 2

    names_blob = b"".join(s.encode() + b"\x00" for s in EXPORTS)
    name_rvas = []
    off = str_off
    rdata_rva = 0x2000
    text_rva = 0x1000
    for s in EXPORTS:
        name_rvas.append(rdata_rva + off)
        off += len(s) + 1
    dll_name_rva = rdata_rva + off
    blob = bytearray()
    # export directory
    blob += struct.pack("<IIHHIIIIIII", 0, 0, 0, 0, dll_name_rva, 1,
                        n, n, rdata_rva + func_tab, rdata_rva + name_tab,
                        rdata_rva + ord_tab)
    assert len(blob) == dir_size
    # function address table -> .text stubs
    for i in range(n):
        blob += struct.pack("<I", text_rva + i * len(STUB))
    # name pointer table
    for rva in name_rvas:
        blob += struct.pack("<I", rva)
    # ordinals
    for i in range(n):
        blob += struct.pack("<H", i)
    # strings
    blob += names_blob + DLL_NAME.encode() + b"\x00"
    rdata = bytes(blob)

    # ---- headers
    FILE_ALIGN, SECT_ALIGN = 0x200, 0x1000
    text_raw = 0x200
    rdata_raw = 0x400
    size_of_image = 0x3000

    dos = bytearray(64)
    dos[0:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 0x80)  # e_lfanew -> PE header at 0x80
    dos_msg = b"This program cannot be run in DOS mode.\r\n$"
    pe = bytearray()
    pe += dos + dos_msg
    pe += b"\x00" * (0x80 - len(pe))
    pe += b"PE\x00\x00"
    # COFF header
    pe += struct.pack("<HHIIIHH", 0x8664, 2, 0, 0, 0, 0xF0, 0x210E)
    # Optional header (PE32+)
    opt = bytearray(0xF0)
    struct.pack_into("<HBB", opt, 0, 0x20B, 0, 0)  # magic + linker ver
    struct.pack_into("<I", opt, 16, text_rva)      # AddressOfEntryPoint
    struct.pack_into("<I", opt, 20, text_rva)      # BaseOfCode
    struct.pack_into("<Q", opt, 24, 0x180000000)   # ImageBase
    struct.pack_into("<I", opt, 32, SECT_ALIGN)    # SectionAlignment
    struct.pack_into("<I", opt, 36, FILE_ALIGN)    # FileAlignment
    struct.pack_into("<I", opt, 56, size_of_image)  # SizeOfImage
    struct.pack_into("<I", opt, 60, FILE_ALIGN)   # SizeOfHeaders
    struct.pack_into("<H", opt, 68, 3)             # Subsystem (console)
    struct.pack_into("<H", opt, 70, 0x8160)        # DllCharacteristics
    # DataDirectory[0] = export table
    struct.pack_into("<I", opt, 108, 16)  # NumberOfRvaAndSizes
    struct.pack_into("<II", opt, 112, rdata_rva, len(rdata))
    pe += opt
    # Section headers: .text, .rdata
    for name, vaddr, vsize, raw, rawsz, chars in [
            (b".text", text_rva, len(text), text_raw, FILE_ALIGN,
             0x60000020),
            (b".rdata", rdata_rva, len(rdata), rdata_raw, FILE_ALIGN,
             0x40000040)]:
        pe += struct.pack("<8sIIIIIIHHI", name, vsize, vaddr, rawsz, raw,
                          0, 0, 0, 0, chars)
    pe += b"\x00" * (text_raw - len(pe))
    # .text
    t = bytearray(FILE_ALIGN)
    t[0:len(text)] = text
    pe += t
    # .rdata
    r = bytearray(FILE_ALIGN)
    r[0:len(rdata)] = rdata
    pe += r
    return bytes(pe)


def main():
    import sys
    out = sys.argv[1] if len(sys.argv) > 1 else "EasyAntiCheat.dll"
    with open(out, "wb") as f:
        f.write(build_stub_dll())
    print(f"wrote {out} ({len(build_stub_dll())} bytes, "
          f"{len(EXPORTS)} exports)")


if __name__ == "__main__":
    main()
