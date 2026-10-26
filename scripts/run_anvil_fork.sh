#!/usr/bin/env bash
set -euo pipefail

RPC_URL="${MAINNET_RPC_URL:-https://eth-mainnet.public.blastapi.io}"
BLOCK_NUMBER="${FORK_BLOCK_NUMBER:-19500000}"
PORT="${ANVIL_PORT:-8545}"

echo "Launching Anvil Mainnet Fork on port ${PORT} at pinned block ${BLOCK_NUMBER}..."
anvil --fork-url "${RPC_URL}" --fork-block-number "${BLOCK_NUMBER}" --port "${PORT}"
