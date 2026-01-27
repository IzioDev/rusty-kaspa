# Kaspa Native Asset Covenants (Minter + Token)

**Scope:** This document specifies the _on-chain behavior_ of the two covenant scripts contained in this example and the transaction shapes they expect.

This example demonstrates a “native asset” protocol implemented using **two cooperating covenants**:

- **Minter covenant**: controls **total supply** (minting only, capped).
- **Token covenant**: controls **ownership** and **conservation** (transfer/split/merge without changing total).

The protocol relies on **`covenant_id`** and **covenant bindings** to identify and authorize covenant outputs.

## Mental model

- The **minter** is the “printer” that can mint tokens, up to a cap.
- The **token covenant** is the “wallet logic” that:
  - lets holders transfer tokens,
  - split one token UTXO into several,
  - merge several token UTXOs into one,
  - and **never allows token amount to appear/disappear** during token operations.

Ownership and authority are proven by including a normal transaction input that spends a UTXO locked to the owner/authority address. The covenant checks that the input’s locking script matches the stored owner/authority script.

## Terminology

- **SPK**: ScriptPublicKey (Kaspa output locking data).
- **SPK bytes**: `ScriptPublicKey::to_bytes()` output (includes the SPK version prefix).
- **State-in-script**: covenant state is encoded in the script bytes.
- **Logic tail**: the fixed covenant program appended after the state prefix.
- **Covenant binding**: output metadata that declares:
  - which input authorizes the output (`authorizing_input`)
  - which covenant group it belongs to (`covenant_id`)
- **Authorized output k**: the k-th output authorized by a given input, addressable via covenant opcodes (`OpCovOutputCount`, `OpCovOutputIdx`).
- **Asset id**: the `covenant_id`.

## What exists on-chain

An “asset” in this protocol consists of:

1. **Exactly one live minter UTXO** at any time (the current minter state).
2. **Zero or more token UTXOs**, each carrying an integer token amount.

Each token UTXO represents a balance:

- Token amount is stored in the token covenant script state (`amount` field).
- The **KAS value** (output value) is just a carrier for fees/dust rules and is **not** the token amount.

## Asset identity

### Asset id is `covenant_id`

The stable identifier for the asset is the **`covenant_id`** (a 32-byte hash). It is computed from an outpoint (later will be computed with more scoped data, that will guarantee uniqueness).

All covenant outputs for a given asset **MUST** use the same `covenant_id` in their covenant binding (`output.covenant`).

## Covenant binding usage

This protocol depends on transaction outputs carrying covenant binding info, and on txscript covenant opcodes being enabled (post covenant hardfork).

### Binding rules

- **Minter outputs** created by a minter spend: `authorizing_input = 0`
- **Token outputs** created by token split/merge/transfer: `authorizing_input = 1` (the “leader token input”)

This is not arbitrary: the scripts assume these indices.

---

## State encoding (state-in-script)

Both minter and token covenant scripts begin with the same fixed-length **state prefix**, followed by a fixed **logic tail**.

### High-level layout

**Script:**

```
[Push amount (8 bytes, little-endian)]
[Push spk_field (38 bytes)]
[Logic tail (minter or token)]
```

Where:

- `amount`:
  - For **minter**: `remaining_supply`
  - For **token**: `token_amount`
- `spk_field`: A compact encoding of an SPK used for authorization:
  - For **minter**: `authority_spk_bytes`
  - For **token**: `owner_spk_bytes`

### `spk_field` format

`spk_field` is always 38 bytes:

```
spk_field = [len: u8] || [spk_bytes: len bytes] || [0x00 padding to 37 bytes total payload]
```

Notes: `spk_bytes` refers to the full serialized SPK including its version prefix (use `ScriptPublicKey::to_bytes()` in this example).

### Amount encoding constraint

The scripts decode the 8-byte amount using `OpBin2Num` and then do numeric operations. This implementation enforces (at build/parse time) that decoded values **MUST** fit into a signed 64-bit integer (`i64::MAX`), otherwise the protocol is considered invalid.

## Minter script

When validating input 0 (the minter input), the minter script enforces:

1. `tx.input_index == 0`
2. `tx.input_count >= 2`
3. `tx.input[1].spk == authority_spk_bytes` (via SPK field matching)
4. `OpCovOutputCount(0) == 2`
5. Authorized output #0:
   - **MUST** have the same minter logic tail as the current minter input
   - **MUST** keep the same authority SPK field
6. Authorized output #1:
   - **MUST** have the token logic tail (as embedded into the minter script at build time)
   - **MUST** have a valid recipient/owner SPK length (36–37)
7. Minted amount:
   - `minted_amount = token_output.amount`
   - `minted_amount MUST be >= 1`
8. Supply cap:
   - `remaining_supply >= minted_amount`
9. State transition:
   - `next_remaining_supply = remaining_supply - minted_amount`
   - authorized output #0 **MUST** encode `next_remaining_supply` as its amount field

**What this means:**

- You can mint to any recipient by setting the token output’s owner SPK field.
- You cannot mint zero.
- You cannot exceed remaining supply.
- Authority cannot be changed by minting (in this implementation).

## Token transactions (transfer / split / merge)

All token operations are expressed as “split/merge” spends. A simple transfer is just a 1-input → 1-output split/merge with the same token amount and a different owner SPK.

### Required input ordering

Token spends **MUST** be ordered as:

- **Input 0**: authorization input (normal UTXO) locked to the token owner SPK
- **Input 1**: **leader token input** (a token covenant UTXO)
- **Input 2..3** (optional): additional token covenant inputs (same owner, same asset)

### Required output shape and bindings

All outputs in a token transaction **MUST** be token covenant outputs authorized by the leader:

Every output must have covenant binding:

- `authorizing_input = 1`
- `covenant_id = asset_id`

### Script-enforced checks

The token covenant script runs for each token input. It performs full checks **only on the leader input (input index 1)**; other token inputs do minimal checks and then accept, relying on the leader’s validation.

Token script enforces:

**Always (for any token input):**

1. Total input count **MUST** be between 2 and 4 (inclusive).
2. Current input index **MUST** be a token input (i.e. `>= 1`).
3. Leader token input (index 1) **MUST** have the same token logic tail as the currently executing token input.
4. All outputs **MUST** be authorized by input 1.

**Leader input:**

5. All token inputs (indices 1..3 that exist) **MUST** encode the same owner SPK field.
6. Authorization input (index 0) SPK **MUST** match that owner SPK field.
7. Extra token inputs (indices 2..3 that exist) **MUST**:

- match the token logic tail, and
- encode the same `covenant_id` field as the leader’s encoded `covenant_id`

8. Token amounts:
   - All token input amounts **MUST** be >= 1
   - All token output amounts **MUST** be >= 1
9. Output scripts:
   - Each authorized output **MUST** use the token logic tail
   - Each output owner SPK length **MUST** be within bounds (36–37)
10. Conservation:

- `sum(input token amounts) == sum(output token amounts)`

**What this means:**

- Transfers are allowed (owner field changes on outputs).
- Split and merge are allowed (amounts can redistribute across outputs).
- Burning is not allowed (cannot reduce total): but can send to the burn address.
- Minting is not allowed (cannot increase total).
- A single authorization input can authorize up to 3 token inputs, but only if they share the same owner.

## Limits (hard-coded by this implementation)

- Token transaction **max token inputs**: 3
- Token transaction **max outputs**: 3
- Amount fields are 8-byte little-endian but **MUST** fit signed i64 for script arithmetic.

## Reference implementation layout (where to look)

- `src/covenants.rs`
  - Builds minter/token scripts.
  - Builds example mint and token split/merge transactions.
  - Contains the covenant logic (txscript builder).
- `src/script_layout.rs`
  - Defines the exact byte layout and offsets of the state prefix.
- `src/native_asset_vm.rs`
  - Local VM demo that runs covenant validation with `TxScriptEngine`.
  - Includes a helper to build a covenants context for `OpCovOutputCount/Idx`.
- `src/main.rs`
  - wRPC demo that constructs transactions and submits them to testnet. (not working at the moment)

## Running the examples

### Local VM demo (no network)

Runs deploy + mint + split + merge locally in the txscript engine:

```bash
cargo run -p kaspa-wrpc-covenants-native-asset --bin kaspa-wrpc-covenants-native-asset-vm
```

### wRPC testnet demo (needs funds)

Broadcasts deploy + mints + transfer on testnet using a provided mnemonic:

```bash
cargo run -p kaspa-wrpc-covenants-native-asset -- <mnemonic>
```

Notes:

- The demo derives a few addresses at fixed indexes (funding, genesis, authority, new owner).
- You must fund the “funding” address on the selected network for the flow to succeed.

## Indexing and wallet notes

- **Do not identify the asset by script hash.** Scripts change every spend because state is embedded in the script.
- Use **`covenant_id`** from covenant binding as the canonical asset id.
- To extract token balances:
  - Find token covenant outputs for a given `covenant_id`
  - Parse the token amount from the state prefix’s `amount` field
  - Parse the owner SPK bytes from `spk_field`

## Known simplifications

These are intentional simplifications in this example:

- Token spends cannot include non-token outputs (no explicit “change” output).
- Authority cannot be rotated (minter requires the authority SPK field to remain constant).
- No metadata standard (name, ticker, decimals, URI, etc.).
- No allowance/approval model; only direct owner authorization via an auth input.
