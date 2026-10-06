#!/usr/bin/env bash
# Builds smart_position_lock.dll and assembles the mod folder dist/smart_position_lock/ and
# dist/smart_position_lock-<version>.zip. On Windows it uses the native toolchain; elsewhere it
# cross-compiles (target x86_64-pc-windows-gnu, needs the mingw-w64 linker).
set -euo pipefail
cd "$(dirname "$0")/.."
id=smart_position_lock

cargo test --quiet
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) cargo build --release; dll="target/release/$id.dll" ;;
    *)
        target=x86_64-pc-windows-gnu
        rustup target add "$target" >/dev/null 2>&1 || true
        cargo build --release --target "$target"
        dll="target/$target/release/$id.dll"
        ;;
esac

version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
py=$(command -v python3 || command -v python)
info_version=$("$py" -c "import json;print(json.load(open('package/mod.mod_info', encoding='utf-8'))['version'])")
if [ "$version" != "$info_version" ]; then
    echo "Cargo.toml version $version != package/mod.mod_info version $info_version" >&2
    exit 1
fi

out="dist/$id"
rm -rf "$out"
mkdir -p "$out"
cp "$dll" package/mod.mod_info package/thumbnail.png "$out/"

"$py" - "$id" "$version" <<'PY'
import os, sys, zipfile
mod, version = sys.argv[1], sys.argv[2]
path = f"dist/{mod}-{version}.zip"
with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
    for root, _, files in sorted(os.walk(f"dist/{mod}")):
        for name in sorted(files):
            full = os.path.join(root, name)
            z.write(full, os.path.join(mod, os.path.relpath(full, f"dist/{mod}")))
print(path)
PY
