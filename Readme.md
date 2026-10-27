# ⚡ Sub-10µs Ethereum Atomic Backrun MEV Engine

[![Rust](https://img.shields.io/badge/Rust-1.82%2B-orange.svg?logo=rust)](https://www.rust-lang.org/)
[![Solidity](https://img.shields.io/badge/Solidity%20%2F%20Yul-0.8.26-363636.svg?logo=solidity)](https://soliditylang.org/)
[![Core Tick-to-Trade](https://img.shields.io/badge/Core%20Tick--to--Trade-8.82%20%C2%B5s-00e676.svg)](#performance-benchmarks-phase-1-vs-phase-2)
[![L1 State Access](https://img.shields.io/badge/L1%20Cache%20Pool%20Read-1.52%20ns-00b0ff.svg)](#1-lock-free-rcu--64-byte-l1-cache-aligned-memory-layout-cratesstate)
[![Zero-RPC EVM Sim](https://img.shields.io/badge/Zero--RPC%20revm%20Sim-5.01%20%C2%B5s-aa00ff.svg)](#3-zero-rpc-pre-warmed-evm--ssd-snapshot-engine-cratessimulator)
[![Tests](https://img.shields.io/badge/Tests-44%20Rust%20%7C%2010%20Foundry%20Passing-success.svg)](#reproduction--verification-guide)

An institutional-grade, ultra-low-latency Ethereum atomic backrun arbitrage engine engineered in **Rust**, **Solidity**, and **Inline Yul Assembly**. 

Designed from the ground up to eliminate heap allocations, lock contention, pointer chasing, and network RPC stalls on the critical path, this engine achieves an end-to-end **Core Tick-to-Trade latency of `8.82 µs` (`8,815 ns`)**—from raw pending transaction ingestion through multi-hop cycle discovery, optimal input sizing ($x^*$), zero-RPC in-process `revm` simulation, and bit-packed Yul calldata construction.

---

## 📊 Performance Benchmarks (Phase 1 vs. Phase 2)

All micro-benchmarks are statistically verified using `criterion` in `--release` (`lto = "fat"`, `codegen-units = 1`) alongside Foundry gas profiling against pinned Ethereum Mainnet state (Block `19,500,000`).

### 1. End-to-End Hot Pipeline Breakdown (`bin/bot/benches/tick_to_trade.rs`)

| Pipeline Stage / Composite Benchmark | Measured Mean Latency | Target Budget | Margin / Speedup | Critical-Path Systems Mechanism |
| :--- | :--- | :--- | :--- | :--- |
| **Stage 1: Calldata Decode & Pool ID Lookup** | **`134.30 ns`** | `< 2.00 µs` | **14.8x under budget** | Zero-copy router slice parser + `FxHashMap` two-way `PoolId(u16)` registry |
| **Stage 2: Cycle Lookup & Optimal Sizing ($x^*$)** | **`3.13 µs`** | `< 4.00 µs` | **1.28x under budget** | $O(1)$ inverted line-graph `CycleTable` + closed-form / Golden Section over `&[HotPoolState]` |
| **Stage 3: Zero-RPC EVM Simulation (`revm`)** | **`5.01 µs`** | `< 15.00 µs` | **3.00x under budget** | RAM-resident `PrewarmedDB` + `119 ns` Copy-on-Write snapshot branching |
| **Stage 4: Stack Yul Calldata & Nonce Load** | **`3.04 ns`** | `< 50.00 ns` | **16.4x under budget** | `[u8; 1024]` stack buffer (`2.52 ns`) + `AtomicU64` acquire load (`506.72 ps`) |
| **🔥 Core Tick-to-Trade Decision Pipeline** | **`8.82 µs` (`8,815 ns`)** | `< 50.00 µs` | **5.67x faster than target** | Complete zero-allocation hot path ready for cryptographic signing |
| **Full Wire-Ready Signed Bundle (w/ 2x ECDSA)** | **`168.26 µs`** | `< 250.00 µs` | **1.48x under budget** | Includes `secp256k1` tx signing + EIP-191 `X-Flashbots-Signature` + shared `Bytes` payload |

---

### 2. Subsystem Micro-Benchmarks: Phase 1 Baseline vs. Phase 2 Pinnacle

| Subsystem Operation | Phase 1 Baseline | Phase 2 Pinnacle (`v2.0`) | Speedup / Reduction | Architectural Upgrade |
| :--- | :--- | :--- | :--- | :--- |
| **Pool State Read (`mev-state`)** | `24.17 ns` (`DashMap` + SipHash)[cite: 15] | **`1.52 ns`** (Direct L1 slice) | **15.8x faster** | `#[repr(C, align(64))]` `HotPoolState` + lock-free `ArcSwap` RCU[cite: 12, 14] |
| **16-Thread Reader + Writer Contention** | Lock contention jitter | **`16.12 ns`** | **Wait-free readers** | Epoch-protected `ArcSwap<HotMarketState>` pointer load[cite: 12] |
| **Touched-Pool Cycle Discovery** | `~60.00 µs` (Runtime bounded DFS)[cite: 9] | **`1.09 ns`** ($O(1)$ slice index) | **>50,000x faster** | Pre-computed `CycleTable` (`Vec<SmallVec<[u16; 32]>>`)[cite: 9, 13] |
| **Single Multi-Hop Cycle Simulation** | `~1.40 µs` (Heap + `Address` lookup) | **`71.52 ns`** | **19.5x faster** | Pure integer math over contiguous `&[HotPoolState]` |
| **Full Touched-Pool Candidate Sizing** | `89.33 µs` | **`12.58 µs`** (`2.88 µs` / cycle) | **7.10x faster** | Closed-form V2 analytical solver + zero-alloc Golden Section[cite: 8] |
| **EVM Storage Lookup (`SLOAD`)** | Network RPC / `RwLock<HashMap>` | **`20.07 ns`** | **Zero locks / Zero I/O** | `PrewarmedDB` flat composite `(Address, U256)` `FxHashMap` |
| **Sandbox State Branching (`snapshot()`)** | Full state clone | **`119.15 ns`** | **Copy-on-Write** | Lightweight journal checkpointing over immutable `PrewarmedDB`[cite: 7] |
| **Full EVM Backrun Execution (`revm`)** | `4.44 s` (Cold) / `99.45 µs` (Warm) | **`5.01 – 5.67 µs`** | **17.5x faster** (vs warm) | Bitwise `Slot 8` (V2) & `Slot 0` (V3) mathematical synthesis |
| **Calldata Serialization (`mev-executor`)** | `356.99 ns` (`SolCall::abi_encode`) | **`2.52 ns`** (Yul) / **`24.66 ns`** (ABI) | **141.4x faster** | Fixed stack-allocated `RawCalldataBuilder` (`[u8; 1024]`) |
| **Account Nonce Retrieval** | RPC `eth_getTransactionCount` / Mutex | **`506.72 ps` (`0.51 ns`)** | **Sub-nanosecond** | Lock-free `AtomicNonceManager` (`Ordering::Acquire`) |
| **2-Hop Calldata Wire Footprint** | `580 bytes` (Standard ABI) | **`166 bytes`** (Compact Yul) | **71.4% smaller** | 36-byte header + 65 bytes/hop bit-packed layout |
| **On-Chain Flash Arbitrage Gas** | `618,258 gas` (Solidity ABI) | **`616,569 gas`** (Yul `fallback`) | **-1,689 gas saved** | Zero-selector inline Yul `calldataload` bitwise decoder |

---

## 🏗️ System Architecture

```text
                                 IN-PROCESS / MEMPOOL INGESTION
   ┌───────────────────────────────────────────┬───────────────────────────────────────────┐
   │  Paradigm reth ExEx (InMemoryTxSource)    │   WebSocket / Unix IPC Mempool Stream     │
   │  Zero-Copy In-Process Notification        │   Uniswap V2/V3, Sushi, Universal Router  │
   └─────────────────────┬─────────────────────┴─────────────────────┬─────────────────────┘
                         └─────────────────────┬─────────────────────┘
                                               │ (~134 ns: Zero-Copy Decode + Address -> PoolId(u16))
                                               ▼
   ┌───────────────────────────────────────────────────────────────────────────────────────┐
   │ 1. LOCK-FREE RCU MARKET STATE (crates/state)                                          │
   │    • ArcSwap<HotMarketState>: Wait-free epoch pointer load (~11.4 ns)                 │
   │    • Vec<HotPoolState>: #[repr(C, align(64))] — 1 Pool = Exactly 1 L1 Cache Line      │
   │    • Direct Slice Indexing: guard.pools[pool_id.0 as usize] -> 1.52 ns (5 CPU cycles) │
   └───────────────────────────────────────────┬───────────────────────────────────────────┘
                                               │
                                               ▼
   ┌───────────────────────────────────────────────────────────────────────────────────────┐
   │ 2. PRE-COMPUTED LINE-GRAPH ROUTER & SIZING ENGINE (crates/strategy)                   │
   │    • Inverted Index: pool_to_cycles[pool_id] -> &[CycleId] in 1.09 ns (O(1), 0 DFS)   │
   │    • HotCycle (36B) & HotCycleHop (8B): Stack-resident, pre-computed zero_for_one     │
   │    • Pure V2 2-Hop: Closed-form analytical x* in ~15 ns                               │
   │    • Mixed V2/V3 & 3-Hop: 16-step Golden Section over &[HotPoolState] in 2.88 µs      │
   └───────────────────────────────────────────┬───────────────────────────────────────────┘
                                               │
                                               ▼
   ┌───────────────────────────────────────────────────────────────────────────────────────┐
   │ 3. ZERO-RPC PRE-WARMED EVM SANDBOX (crates/simulator)                                 │
   │    • PrewarmedDB: Lock-free Arc<FxHashMap<(Address, U256 U256),>> (20.07 ns SLOAD)    │
   │    • Bitwise Storage Synthesis: Syncs V2 Slot 8 & V3 Slot 0/4 in <100 ns without RPC  │
   │    • Offline SSD Persistence: Bincode state snapshots load in <5 ms                   │
   │    • CoW Snapshot Branching (119 ns) + Full EVM Backrun Verification (5.01 µs)        │
   └───────────────────────────────────────────┬───────────────────────────────────────────┘
                                               │
                                               ▼
   ┌───────────────────────────────────────────────────────────────────────────────────────┐
   │ 4. ZERO-ALLOCATION CALLDATA & MULTI-RELAY EXECUTOR (crates/executor & contracts/)     │
   │    • RawCalldataBuilder: Encodes 166B Yul payload on stack [u8; 1024] in 2.52 ns      │
   │    • AtomicNonceManager: Sub-nanosecond (506 ps) lock-free nonce management           │
   │    • FastBundleSigner: Single-serialize JSON-RPC Bytes + EIP-191 X-Flashbots-Signature│
   │    • Concurrent Fan-Out: Flashbots, Titan, Beaverbuild, Builder0x69                   │
   └───────────────────────────────────────────────────────────────────────────────────────┘
```

---

## 🔬 Deep-Dive: Core Systems Engineering Innovations

### 1. Lock-Free RCU & 64-Byte L1 Cache-Aligned Memory Layout (`crates/state`)
Standard concurrent maps (`DashMap` / `RwLock<HashMap<Address, PoolState>>`) incur 20-byte cryptographic SipHash overhead, bucket lock acquisitions, and heap pointer chasing across cache lines (`24.17 ns` per read)[cite: 1, 15].
* **Contiguous Integer IDs:** Cold ingestion maps 20-byte `Address` identifiers into sequential `TokenId(u16)` and `PoolId(u16)` indices via a bidirectional `FxHashMap` registry[cite: 13].
* **Single Cache-Line Packing (`#[repr(C, align(64))]`):** Every pool's hot pricing state (`HotPoolState`) is packed into **exactly 64 bytes**—matching the hardware L1 data cache line size of modern x86_64/ARM64 CPUs[cite: 14]:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[repr(C, align(64))]
  pub struct HotPoolState {
      pub slot0_lo: u128,              // 16B (0..16):  V2 reserve0 | V3 sqrtPriceX96 (low 128b)
      pub slot1_liquidity: u128,       // 16B (16..32): V2 reserve1 | V3 liquidity (uint128)
      pub sqrt_price_hi: u64,          // 8B  (32..40): V3 sqrtPriceX96 (high 32b)
      pub tick: i32,                   // 4B  (40..44): V3 current tick (int24)
      pub fee_pips: u32,               // 4B  (44..48): Pool fee tier in pips
      pub block_timestamp_last: u32,   // 4B  (48..52): Last sync timestamp
      pub token0: TokenId,             // 2B  (52..54): Contiguous u16 Token0 ID
      pub token1: TokenId,             // 2B  (54..56): Contiguous u16 Token1 ID
      pub pool_id: PoolId,             // 2B  (56..58): Contiguous u16 Pool ID
      pub variant: u8,                 // 1B  (58..59): 0 = UniswapV2, 1 = UniswapV3
      pub _padding: [u8; 5],           // 5B  (59..64): Explicit alignment -> 64 Bytes Total
  }
  ```
* **Wait-Free RCU Snapshots:** Readers grab an epoch-protected `ArcSwap` guard once per tick and index `&hot.pools[pool_id]` in **`1.52 ns`** with zero locks and zero false sharing[cite: 12, 14].

### 2. $O(1)$ Pre-Computed Line-Graph Cycles & Exact Integer Sizing (`crates/strategy`)
* **Inverted Cycle Index (`CycleTable`):** Enumerates all 2-hop and 3-hop `WETH`-anchored cycles at startup into 36-byte `HotCycle` structs (containing four 8-byte `HotCycleHop` entries with pre-evaluated `zero_for_one` flags)[cite: 9]. When a victim swap touches `PoolId(k)`, `pool_to_cycles[k]` returns all affected candidate cycles in **`1.09 ns`**.
* **Analytical + Golden Section Sizing ($x^*$):**
  * **Pure 2-Hop V2 Cycles:** Solves the exact closed-form derivative root over virtual reserves $E_{in}, E_{out}$ in ~15 ns[cite: 8]:
    $$x^* = \frac{\sqrt{E_{in} \cdot E_{out} \cdot 997 \cdot 1000} - E_{in} \cdot 1000}{997}$$
  * **3-Hop & Mixed V2/V3 Cycles:** Runs bounded Golden Section Search directly over `&[HotPoolState]` using $Q64.96$ fixed-point math (`get_amount_out_v3_hot`), evaluating a full cycle in **`71.52 ns`** and converging to 1-wei parity in **`2.88 µs`**[cite: 8].

### 3. Zero-RPC Pre-Warmed EVM & SSD Snapshot Engine (`crates/simulator`)
* **Eliminating the Cold-Storage Trap:** Replaces lazy RPC querying during simulation with `PrewarmedDB`—an immutable, lock-free `FxHashMap` backing store where `SLOAD` resolves in **`20.07 ns`** and a complete EVM transaction executes in **`5.01 µs`**.
* **Bitwise Storage Slot Synthesis:** Pool reserve updates from `HotMarketState` are mathematically packed directly into EVM storage without RPC calls:
  $$\text{Uniswap V2 Slot 8} = \text{reserve}_0 \;\vert\; (\text{reserve}_1 \ll 112) \;\vert\; (\text{timestamp} \ll 224)$$
* **Offline SSD Snapshot Engine:** Serializes warmed mainnet accounts, proxy bytecodes (e.g., USDC `delegatecall` implementation), and storage slots via `bincode` for `<5 ms` cold-boot reloads and 100% deterministic offline testing.

### 4. Inline Yul Executor & Bit-Packed Calldata (`contracts/` & `crates/executor`)
* **71.4% Calldata Compression (`2.52 ns` Encoding):** `RawCalldataBuilder::encode_compact_packed` bypasses Solidity's 32-byte ABI padding, writing a tightly packed 36-byte header + 65-byte hop descriptor directly into a stack array `[u8; 1024]`.
* **Zero-Selector Yul `fallback()`:** `MevExecutor.sol` decodes the compact payload using raw `calldataload` and bitwise shifts (`shr`, `and`), executing Uniswap V2 flash swaps (`Mode B`), V3 callbacks, strict profit verification (`netProfit >= minProfit`), and atomic `block.coinbase` builder bribes while saving **1,689 gas** per execution.

---

## 📂 Workspace Structure

```text
MEV_arbitary_bot/
├── Cargo.toml                  # Workspace manifest (release profile: fat LTO, codegen-units = 1)
├── contracts/                  # Foundry project (Solidity 0.8.26 & Inline Yul)
│   ├── src/MevExecutor.sol     # Mode A (Direct), Mode B (Flash Swap), Mode C (Compact Yul fallback)
│   └── test/                   # 10 Foundry unit, security, gas, & mainnet fork invariant tests
├── crates/
│   ├── types/                  # TokenId(u16), PoolId(u16), 64B HotPoolState, 8B HotCycleHop
│   ├── collector/              # WS/IPC ingestion, V2/V3/UniversalRouter decoders, reth ExEx adapter
│   ├── state/                  # Lock-free ArcSwap<HotMarketState>, FxHash IdRegistry, Multicall3 sync
│   ├── simulator/              # In-process revm ForkedSandbox, PrewarmedDB, bincode SSD snapshots
│   ├── strategy/               # O(1) CycleTable, 256-bit AMM pricing, closed-form & Golden Section x*
│   └── executor/               # Stack RawCalldataBuilder, AtomicNonceManager, EIP-191 multi-relay client
└── bin/
    └── bot/                    # Dual-mode BotEngine orchestrator & flagship tick-to-trade benchmarks
```

---

## 🚀 Reproduction & Verification Guide

### Prerequisites
* **Rust:** `1.82+` (`rustup update stable`)
* **Foundry:** `forge` & `anvil` (`foundryup`)
* **Environment:** Copy `.env.example` to `.env` and set `ETH_HTTP_RPC_URL` (e.g., Alchemy/Infura Mainnet URL for live fork tests).

### 1. Run All 44 Rust Workspace Tests & 10 Foundry Contract Suites
```bash
# Run the complete Rust test suite across all 7 workspace crates
cargo test --workspace

# Run the Foundry smart contract test suite with gas reporting
forge test --root contracts --gas-report
```

### 2. Run the Side-by-Side Phase 1 vs. Phase 2 Mainnet Fork E2E Pipeline
Executes a real 200 WETH whale swap dislocation on forked Ethereum Mainnet (Block `19,500,000`), verifying the **728x live-fork speedup** and **100% bit-for-bit net profit parity (`0.019026 ETH` net profit, `0.2054 ETH` builder bribe)**:
```bash
cargo test -p mev-bot --test e2e_pipeline_test --release -- --nocapture
```

### 3. Run the Criterion Nanosecond & Microsecond Benchmark Suites
```bash
# 1. Flagship End-to-End Tick-to-Trade Benchmark (8.82 µs core pipeline)
cargo bench -p mev-bot --bench tick_to_trade

# 2. L1 Cache-Aligned RCU State Benchmark (1.52 ns slice lookup vs 24.17 ns DashMap)
cargo bench -p mev-state --bench market_state_latency

# 3. Pre-Computed CycleTable & Hot Sizing Benchmark (1.09 ns lookup, 2.88 µs sizing)
cargo bench -p mev-strategy --bench strategy_latency

# 4. Zero-RPC PrewarmedDB revm Benchmark (20.07 ns SLOAD, 5.67 µs full EVM tx)
cargo bench -p mev-simulator --bench simulator_latency

# 5. Stack Calldata & Atomic Nonce Benchmark (2.52 ns Yul encode, 506 ps nonce load)
cargo bench -p mev-executor --bench executor_latency
```

---

## 🛡️ Security & Production Safety Invariants

* **Zero Principal Risk (Flash Swap Mode):** Capital is borrowed optimistically via Uniswap V2 `swap()` callbacks and repaid within the same atomic execution frame.
* **On-Chain Profit Enforcement:** `MevExecutor.sol` reverts the entire transaction if realized post-swap WETH balance does not strictly exceed `amountIn + minProfit + bribeAmount`.
* **Callback Spoof Protection:** Both `uniswapV2Call` and `uniswapV3SwapCallback` enforce strict `msg.sender == activeCallbackPool` and `owner` access controls.
* **Next-Block Pinning (`blockNumber = N + 1`):** Bundles are cryptographically bound to the immediate next block so delayed bundles automatically drop rather than executing against stale reserves.