#!/bin/bash
#
# Drives the language server over stdio the way Zed does -- initialize, open a document,
# run the export command -- and asserts a real PNG comes out the other end. Catches
# breakage in the Mermaid CLI, in the LSP wiring, and in the render path that a unit
# test cannot see.
#
# Uses the export command rather than preview, because preview also tries to open an
# editor tab, which has no meaning on a CI runner.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LSP="${1:-$REPO_ROOT/target/release/mermaid-quicklook-lsp}"

if [[ ! -x "$LSP" ]]; then
    echo "language server not found at $LSP" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

DIAGRAM="$WORK/fixture.mmd"
EXPECTED="$WORK/fixture.png"

# The bare `%%` on line 2 is the regression guard: Mermaid rejects a comment marker
# with nothing after it, and the server is expected to strip it before rendering.
cat > "$DIAGRAM" <<'EOF'
%% fixture — exercises comments, subgraphs and links
%%
flowchart TB
    subgraph group["a subgraph"]
        direction TB
        A[Start] --> B{Branch}
    end
    B -->|yes| C[Done]
    B -->|no| A
EOF

uri="file://$DIAGRAM"

send() {
    local body="$1"
    printf 'Content-Length: %d\r\n\r\n%s' "$(printf '%s' "$body" | wc -c)" "$body"
}

{
    send "$(jq -nc --arg uri "$uri" \
        '{jsonrpc:"2.0",id:1,method:"initialize",params:{processId:null,rootUri:null,capabilities:{}}}')"
    send "$(jq -nc '{jsonrpc:"2.0",method:"initialized",params:{}}')"
    send "$(jq -nc --arg uri "$uri" --rawfile text "$DIAGRAM" \
        '{jsonrpc:"2.0",method:"textDocument/didOpen",params:{textDocument:{uri:$uri,languageId:"mermaid",version:1,text:$text}}}')"
    send "$(jq -nc --arg uri "$uri" \
        '{jsonrpc:"2.0",id:2,method:"workspace/executeCommand",params:{command:"mermaid.exportPng",arguments:[{uri:$uri}]}}')"

    # Hold stdin open while mmdc works; it boots Chromium and takes a few seconds.
    for _ in $(seq 1 60); do
        [[ -f "$EXPECTED" ]] && break
        sleep 1
    done
} | "$LSP" || true

if [[ ! -f "$EXPECTED" ]]; then
    echo "FAIL: the server produced no PNG at $EXPECTED" >&2
    exit 1
fi

if ! file "$EXPECTED" | grep -q 'PNG image data'; then
    echo "FAIL: output is not a PNG: $(file "$EXPECTED")" >&2
    exit 1
fi

echo "ok: rendered $(file -b "$EXPECTED"), $(wc -c < "$EXPECTED") bytes"
