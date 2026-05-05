#!/usr/bin/env python3
"""Verify a built dxpdf wheel has FreeType properly embedded.

With ``skia-safe[embed-freetype]`` enabled, FreeType is statically linked
into Skia. The published wheel must therefore:

  1. not list ``libfreetype.so.6`` (Linux) or any ``libfreetype`` dylib
     (macOS) as a dynamic dependency,
  2. not bundle a separate ``libfreetype`` shared library (auditwheel /
     delocate copy in any non-whitelisted dep — if FreeType is embedded,
     none should appear),
  3. contain no UNDEFINED ``FT_*`` symbols in the extension module
     (defended-in-depth: catches the case where Skia's prebuilt was
     linked against system FreeType despite the feature flag).

Re-introducing any of the above brings back the "undefined symbol:
FT_Palette_Data_Get" error on hosts with older system FreeType.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


def linux_dt_needed(so: Path) -> list[str]:
    out = subprocess.check_output(["readelf", "-d", str(so)], text=True)
    needed = []
    for line in out.splitlines():
        m = re.search(r"\(NEEDED\).*\[([^\]]+)\]", line)
        if m:
            needed.append(m.group(1))
    return needed


def linux_undefined_ft_symbols(so: Path) -> list[str]:
    out = subprocess.check_output(
        ["nm", "-D", "--undefined-only", str(so)], text=True
    )
    syms = []
    for line in out.splitlines():
        parts = line.split()
        if parts and parts[-1].startswith("FT_"):
            syms.append(parts[-1])
    return syms


def macos_otool_deps(dylib: Path) -> list[str]:
    out = subprocess.check_output(["otool", "-L", str(dylib)], text=True)
    deps = []
    for line in out.splitlines()[1:]:
        line = line.strip()
        if line:
            deps.append(line.split(" ", 1)[0])
    return deps


def fail(msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def verify(wheel: Path) -> None:
    if not wheel.is_file():
        fail(f"{wheel} not found")

    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        with zipfile.ZipFile(wheel) as zf:
            zf.extractall(root)

        ext_candidates = [
            p
            for p in [*root.rglob("dxpdf*.so"), *root.rglob("dxpdf*.dylib")]
            if "dxpdf.libs" not in p.parts
        ]
        if not ext_candidates:
            fail(f"no dxpdf extension module found in {wheel.name}")
        ext = ext_candidates[0]

        print(f"verifying {wheel.name} :: {ext.relative_to(root)}")

        bundled = list(root.rglob("libfreetype*"))
        if bundled:
            for b in bundled:
                print(f"  bundled: {b.relative_to(root)}")
            fail(
                "wheel bundles libfreetype — embed-freetype not in effect "
                "(auditwheel/delocate copied a system FreeType in)"
            )

        if sys.platform.startswith("linux"):
            needed = linux_dt_needed(ext)
            ft_needed = [n for n in needed if "freetype" in n.lower()]
            if ft_needed:
                print(f"  DT_NEEDED: {needed}")
                fail(f"{ext.name} dynamically links to {ft_needed}")
            undef_ft = linux_undefined_ft_symbols(ext)
            if undef_ft:
                shown = ", ".join(undef_ft[:5])
                more = f" (+{len(undef_ft) - 5} more)" if len(undef_ft) > 5 else ""
                fail(f"{ext.name} has unresolved FT_* symbols: {shown}{more}")
        elif sys.platform == "darwin":
            deps = macos_otool_deps(ext)
            ft_deps = [d for d in deps if "freetype" in d.lower()]
            if ft_deps:
                fail(f"{ext.name} dynamically links to {ft_deps}")
        else:
            fail(f"unsupported platform: {sys.platform}")

        print(f"OK: {wheel.name} — FreeType is embedded")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("wheels", type=Path, nargs="+", help="path(s) to .whl files")
    args = ap.parse_args()
    for w in args.wheels:
        verify(w)


if __name__ == "__main__":
    main()
