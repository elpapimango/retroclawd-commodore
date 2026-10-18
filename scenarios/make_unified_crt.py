#!/usr/bin/env python3
"""Wraps crystalct's Unified Diagnostic Cartridge image (software/diagnostics/unified_32k.bin: the standard diagnostic as the first
8K, the modified dead test as the second 8K) in a .crt of retroclawd's own hardware type 0xC640, the way the rc-suite C64
diagnostics case does, so a scenario can load it with --cartridge.

    make_unified_crt.py unified_32k.bin OUT.crt
"""
import struct
import sys

img = open(sys.argv[1], "rb").read()
crt = b"C64 CARTRIDGE   " + struct.pack(">I", 0x40) + struct.pack(">H", 0x0100) + struct.pack(">H", 0xC640)
crt += bytes([1, 0]) + bytes(6) + b"UNIFIED DIAG" + bytes(20)
for load, half in ((0x8000, img[:0x2000]), (0xE000, img[0x2000:0x4000])):
    crt += b"CHIP" + struct.pack(">I", 16 + len(half)) + struct.pack(">HHHH", 0, 0, load, len(half)) + half
open(sys.argv[2], "wb").write(crt)
