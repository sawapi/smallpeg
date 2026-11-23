#!/usr/bin/env bash
# Build the npm package: the bindings crate in wasm/ compiled to
# WebAssembly, with JavaScript glue for Node (npm/node) and for browsers
# and bundlers (npm/web), the same again for the module behind
# smallpeg/jxl (npm/jxl-node, npm/jxl-web), and the TypeScript API on top
# (npm/dist).
# Needs the dev dependencies: `npm ci` in npm/ first.
set -euo pipefail
cd "$(dirname "$0")/.."

WANT=$(grep '^wasm-bindgen = ' wasm/Cargo.toml | cut -d'"' -f2 | tr -d '=')
HAVE=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [[ "$HAVE" != "$WANT" ]]; then
  echo "wasm-bindgen CLI $WANT is required (found: ${HAVE:-none})." >&2
  echo "Install it with: cargo install wasm-bindgen-cli --version $WANT --locked" >&2
  exit 1
fi

CRATE=$(grep '^version = ' Cargo.toml | head -1 | cut -d'"' -f2)
PKG=$(node -p "require('./npm/package.json').version")
if [[ "$CRATE" != "$PKG" ]]; then
  echo "npm/package.json version $PKG does not match Cargo.toml $CRATE" >&2
  exit 1
fi
if [[ ! -x npm/node_modules/.bin/tsc ]]; then
  echo "TypeScript is missing; run npm ci in npm/ first." >&2
  exit 1
fi

# 128-bit SIMD lets the compiler vectorize loops (decoding is about a third
# faster); every current browser and Node 22 support it. Added to RUSTFLAGS
# rather than the per-target variable, which RUSTFLAGS would override.
# With --target, RUSTFLAGS applies to the wasm build only, not to the host.
# CARGO_ENCODED_RUSTFLAGS, if set, overrides both and drops SIMD.
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+simd128" \
  cargo build --manifest-path wasm/Cargo.toml --release --target wasm32-unknown-unknown
WASM=wasm/target/wasm32-unknown-unknown/release/smallpeg_wasm.wasm

rm -rf npm/node npm/web npm/jxl-node npm/jxl-web
wasm-bindgen --target nodejs --out-dir npm/node "$WASM"
wasm-bindgen --target web --out-dir npm/web "$WASM"

# The module behind smallpeg/jxl: the same crate built with its `jxl`
# feature, which holds only the JPEG XL function, so neither entry point
# loads the other's code. It replaces the file at $WASM.
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+simd128" \
  cargo build --manifest-path wasm/Cargo.toml --release --target wasm32-unknown-unknown --features jxl
wasm-bindgen --target nodejs --out-dir npm/jxl-node --out-name smallpeg_jxl_wasm "$WASM"
wasm-bindgen --target web --out-dir npm/jxl-web --out-name smallpeg_jxl_wasm "$WASM"

# The web glue is an ES module; say so, or Node reparses it with a warning.
echo '{ "type": "module" }' > npm/web/package.json
echo '{ "type": "module" }' > npm/jxl-web/package.json

# Shrink further when Binaryen's wasm-opt is installed (optional).
if command -v wasm-opt >/dev/null; then
  for f in npm/node/smallpeg_wasm_bg.wasm npm/web/smallpeg_wasm_bg.wasm \
    npm/jxl-node/smallpeg_jxl_wasm_bg.wasm npm/jxl-web/smallpeg_jxl_wasm_bg.wasm; do
    wasm-opt -O3 --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int \
      --enable-sign-ext --enable-reference-types --enable-multivalue \
      --enable-mutable-globals -o "$f" "$f"
  done
fi

rm -rf npm/dist
npm/node_modules/.bin/tsc -p npm

cp LICENSE-MIT LICENSE-APACHE THIRD_PARTY.md npm/
ls -l npm/node/*.wasm npm/web/*.wasm npm/jxl-node/*.wasm npm/jxl-web/*.wasm
echo "npm package ready in npm/"
