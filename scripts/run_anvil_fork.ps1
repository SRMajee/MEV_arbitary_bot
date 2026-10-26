param (
    [string]$RpcUrl = $env:MAINNET_RPC_URL,
    [int64]$BlockNumber = 19500000,
    [int]$Port = 8545
)

if (-not $RpcUrl) {
    $RpcUrl = "https://eth-mainnet.public.blastapi.io"
}

Write-Host "Launching Anvil Mainnet Fork on port $Port at pinned block $BlockNumber..."
& "C:\Users\User\.foundry\bin\anvil.exe" --fork-url $RpcUrl --fork-block-number $BlockNumber --port $Port
