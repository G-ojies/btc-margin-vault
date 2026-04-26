# BTC Margin Vault

**Borrow USDC against native Bitcoin collateral on Solana — no bridges, no wBTC.**

A Solana Anchor program that lets a user lock real, native BTC as collateral and borrow USDC against it, with an Ika dWallet acting as the cryptographic owner of the on-Bitcoin UTXO. Liquidation flows authorize the dWallet — via on-chain CPI — to release that BTC back into the protocol's possession, all without ever wrapping or bridging the asset.

## Problem

Solana DeFi treats Bitcoin as a second-class citizen. Every BTC-collateralized position today is either:

- **A wrapped IOU** (wBTC, tBTC, sBTC) — the user trusts a custodian or a bridge, and a bridge exploit zeros the position.
- **A bridge-LP claim** — the user owns a synthetic that drifts from BTC under stress, and reconciliation depends on liveness of the bridge.

Native BTC has no smart-contract layer, so there has been no way to write a Solana program that *directly* controls a Bitcoin UTXO. Margin and lending against BTC therefore live behind a wrapper, and the wrapper is the weakest link.

## Solution

BTC Margin Vault uses Ika's dWallet primitive as the on-chain handle to a real Bitcoin address. A dWallet is a programmable, transferable account whose signing authority is shared between the user and the Ika MPC network via 2PC-MPC — meaning a Solana program can *authorize* a Bitcoin signature without ever holding the private key, and without anyone ever holding the full key.

The vault flow:

1. The user generates a dWallet (Bitcoin-controlling address) through Ika and sends BTC to it.
2. `initialize_vault` records the `dwallet_id` and the dollar value of the collateral on Solana.
3. `borrow_usdc` lets the user borrow up to a 70% LTV against that collateral.
4. If the position falls below 85% health, `liquidate` issues a `approve_message` CPI to the Ika dWallet program — telling the MPC network it is authorized to co-sign a BTC transaction that releases the collateral to the protocol.

No wrapping. No bridge. The BTC stays on Bitcoin the entire time; only the *authority* to move it ever crosses chains.

## How it uses the Ika dWallet CPI

This is the integration that makes the whole design work. Walk through what happens during liquidation:

1. **The vault holds a `dwallet_id`** — a `Pubkey` registered at `initialize_vault` (and updatable via `register_dwallet`). It points at an Ika dWallet account that, in turn, controls a real Bitcoin address.

2. **The vault program is itself a Solana program.** It cannot sign Bitcoin transactions. What it *can* do is sign on-chain instructions to the Ika dWallet program using a PDA derived from its own program ID — the **CPI authority PDA**, seeded by `[b"__ika_cpi_authority"]` (a constant exported by `ika-dwallet-anchor`).

3. **When `liquidate` fires** and the LTV is genuinely above the 85% threshold, the handler builds a `DWalletContext` and invokes `approve_message`:

   ```
   ctx.approve_message(
       coordinator,
       message_approval,
       dwallet,
       payer,
       system_program,
       message_digest,           // hash of the BTC tx that releases collateral
       message_metadata_digest,
       user_pubkey,              // Schnorr/EdDSA pubkey held by the user
       signature_scheme,
       message_approval_bump,
   )?;
   ```

   The CPI is `invoke_signed` with the program's CPI authority PDA, so the Ika program sees the call as authorized by *this program*, not by a user-held key.

4. **The Ika program creates a `MessageApproval` PDA.** This PDA is the on-chain signal to the off-chain Ika MPC network: "the dWallet's program-side authority has approved this exact BTC transaction." The Ika network watches for these approvals, runs 2PC-MPC, and produces a Schnorr signature over the BTC transaction.

5. **The signed BTC transaction is broadcast** to the Bitcoin network by any client — the MPC signature is the only thing that was missing, and now it exists.

The crucial security property: even though `liquidate` can be called by anyone (a liquidator), the Solana program enforces the LTV check *before* the CPI fires. The Ika CPI authority will refuse to authorize any signing the program does not explicitly request, and the program will not request signing unless the LTV ratio actually exceeds 85%. The on-chain LTV check and the off-chain BTC release are therefore atomically gated.

## Architecture

```
                    ┌─────────────────────────┐
                    │      vault owner        │
                    └────────────┬────────────┘
                                 │  initialize / deposit / borrow / repay
                                 ▼
   ┌──────────────────────────────────────────────────────────┐
   │  btc-margin-vault  (Solana / Anchor)                     │
   │                                                          │
   │   VaultState PDA  [b"vault", owner.key()]                │
   │     ├─ dwallet_id                                        │
   │     ├─ btc_collateral_usd                                │
   │     ├─ usdc_borrowed                                     │
   │     └─ is_active                                         │
   │                                                          │
   │   CPI authority PDA  [b"__ika_cpi_authority"]            │
   │     └─ signs Ika CPIs on behalf of this program          │
   └──────────────────────────────┬───────────────────────────┘
                                  │ liquidate(): approve_message CPI
                                  │ invoke_signed via CPI authority PDA
                                  ▼
   ┌──────────────────────────────────────────────────────────┐
   │  Ika dWallet program                                     │
   │  87W54kGYFQ1rgWqMeu4XTPHWXWmXSQCcjm8vCTfiq1oY             │
   │                                                          │
   │   MessageApproval PDA  ◄─── the on-chain "approved" flag │
   └──────────────────────────────┬───────────────────────────┘
                                  │ watched off-chain
                                  ▼
   ┌──────────────────────────────────────────────────────────┐
   │  Ika MPC network  (2PC-MPC, threshold signature)         │
   │   produces a Schnorr signature over the BTC tx           │
   └──────────────────────────────┬───────────────────────────┘
                                  │ signed BTC tx broadcast
                                  ▼
                        ┌────────────────────┐
                        │  Bitcoin network   │
                        │  (native UTXO)     │
                        └────────────────────┘
```

## Instructions

| Instruction | Args | Behavior |
|---|---|---|
| `initialize_vault` | `dwallet_id: Pubkey` | Creates the `VaultState` PDA for the calling owner, binds it to a dWallet, sets balances to zero, marks active. |
| `register_dwallet` | `dwallet_id: Pubkey` | Owner-only. Rebinds the vault to a (possibly new) dWallet. Refuses on inactive vaults. |
| `deposit_collateral` | `amount_usd: u64` | Records additional BTC collateral (in USD terms) against the vault. Caller-trusted USD valuation; an oracle would slot in here in production. |
| `borrow_usdc` | `amount: u64` | Borrows USDC if and only if the resulting LTV stays at or below `LTV_THRESHOLD` (70%). Reverts with `InsufficientCollateral` otherwise. |
| `repay_usdc` | `amount: u64` | Reduces `usdc_borrowed` by `amount` (saturating). |
| `liquidate` | `vault: Pubkey, message_digest: [u8;32], message_metadata_digest: [u8;32], user_pubkey: [u8;32], signature_scheme: u16, message_approval_bump: u8` | Permissionless. If LTV > `LIQUIDATION_THRESHOLD` (85%), invokes the Ika dWallet `approve_message` CPI to authorize releasing the BTC, then zeroes the vault and marks it inactive. |

### Constants

| Name | Value | Meaning |
|---|---|---|
| `LTV_THRESHOLD` | `70` | Maximum LTV percentage allowed at borrow time. |
| `LIQUIDATION_THRESHOLD` | `85` | LTV percentage above which `liquidate` will fire the Ika CPI. |
| `VAULT_SEED` | `b"vault"` | Seed prefix for the vault PDA (combined with `owner.key()`). |
| `IKA_CPI_AUTHORITY_SEED` | `b"__ika_cpi_authority"` | Re-exported from `ika-dwallet-anchor`; seed for this program's CPI authority PDA. |

## Build and test

Requirements:

- Rust toolchain pinned to `1.89.0` (see `rust-toolchain.toml`)
- Anchor CLI `1.0.1`
- A Solana SBF toolchain (Anchor will fetch this on first build)

```bash
# Build the program
anchor build

# Run the test suite (LiteSVM-based, in-process; no validator required)
cargo test --package btc-margin-vault --tests
```

Expected output: `8 passed; 0 failed; 1 ignored`. The single ignored test (`liquidate_above_threshold_succeeds`) exercises the full Ika CPI on the success path and is gated until an Ika program fixture is wired into the LiteSVM harness.

## Network info

| Item | Value |
|---|---|
| `btc-margin-vault` program ID | `6UVY7nskmCBaeKS22E6oh5CkS9TcdcmazHzLAme4YNfB` |
| Ika dWallet devnet program ID | `87W54kGYFQ1rgWqMeu4XTPHWXWmXSQCcjm8vCTfiq1oY` |
| Solana cluster | devnet (`https://api.devnet.solana.com`) |
| Ika dWallet gRPC endpoint | `https://pre-alpha-dev-1.ika.ika-network.net:443` |
| Ika SDK | [`ika-dwallet-anchor`](https://github.com/dwallet-labs/ika-pre-alpha) (pinned, pre-alpha) |
