#!/usr/bin/env python3
# Compute the transitive MSYS2/UCRT64 DLL closure needed to bundle a set of
# executables so they run on a stock Windows box (no MSYS2 install).
import lief, os, sys

BIN = sys.argv[1] if len(sys.argv) > 1 else r"C:\msys64\ucrt64\bin"
roots = sys.argv[2:]
if not roots:
    print("usage: dll-closure.py <ucrt64-bin> <exe-or-dll> [more...]"); sys.exit(2)

def imports_of(path):
    try:
        b = lief.PE.parse(path)
        return [i.name for i in b.imports]
    except Exception as e:
        print("  ! parse failed:", path, e); return []

closure = set()      # DLL filenames (in BIN) required
missing = set()
queue = list(roots)
seen_files = set()

while queue:
    f = queue.pop()
    key = os.path.basename(f).lower()
    if key in seen_files: continue
    seen_files.add(key)
    for imp in imports_of(f):
        name = imp
        cand = os.path.join(BIN, name)
        # only bundle DLLs that live in the MSYS2 bin (skip Windows system DLLs)
        if os.path.exists(cand):
            if name.lower() not in closure:
                closure.add(name.lower())
                queue.append(cand)
        else:
            # track only lib*/msys* that we couldn't find (real gaps), ignore win32
            low = name.lower()
            if low.startswith(("lib", "msys")):
                missing.add(name)

# resolve real-cased filenames present in BIN
present = {fn.lower(): fn for fn in os.listdir(BIN)}
result = sorted(present[c] for c in closure if c in present)
for r in result:
    print("BUNDLE", os.path.join(BIN, r))
if missing:
    for m in sorted(missing):
        print("MISSING", m)
print("TOTAL", len(result), "dlls")
