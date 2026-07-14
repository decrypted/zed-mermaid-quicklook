#!/bin/bash
#
# Builds the language server and puts it on PATH, where the Zed extension looks for it.
# The extension itself is loaded by Zed via `zed: install dev extension` (Zed compiles
# the WASM and the tree-sitter grammar from this directory).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "==> Checking the Mermaid CLI (mmdc)"
if ! command -v mmdc >/dev/null 2>&1; then
    echo "    mmdc not found on PATH."
    echo "    Install it with: npm install -g @mermaid-js/mermaid-cli"
    exit 1
fi
echo "    ok: $(command -v mmdc)"

echo "==> Installing the language server to ~/.cargo/bin"
cargo install --path lsp --force

if ! command -v mermaid-quicklook-lsp >/dev/null 2>&1; then
    echo "    Warning: mermaid-quicklook-lsp is not on PATH." >&2
    echo "    Add ~/.cargo/bin to PATH, or set MERMAID_QUICKLOOK_LSP to its location." >&2
    exit 1
fi
echo "    ok: $(command -v mermaid-quicklook-lsp)"

echo "==> Building the Zed extension (WASM)"
rustup target add wasm32-wasip2 >/dev/null 2>&1 || true
cargo build --lib --target wasm32-wasip2 --release

echo
echo "Done. To load the extension in Zed:"
echo "  1. Ctrl+Shift+P  ->  zed: install dev extension"
echo "  2. Select $REPO_ROOT"
echo
echo "Then open any .mmd file, press Ctrl+. and choose \"Open Mermaid Preview\"."
