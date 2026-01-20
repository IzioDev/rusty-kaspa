# Native Asset Covenants (Minter + Token) — protocol example

This create demonstrates a **native-asset protocol** implemented as a pair of cooperating **covenants**:

- a **Minter covenant** that controls supply, and
- a **Token covenant** that controls token movement and enforces conservation of token quantities.

Every covenant spend carries a **fixed-length transaction payload** that declares the intended state transition.
The covenant scripts validate that payload and require outputs that **commit to the next state**.

> Note: this example uses Kaspa transaction payloads + covenant scripts. The “token quantity” for the asset is
> carried and checked in the payload/history, not in the KAS coin value of the outputs.

---

## Mental model (non-technical)

- **Minter** = the “printer”. It can authorize minting up to a capped supply.
- **Token** = the “wallet layer”. It lets holders split/merge/transfer balances, but never changes the total.

Each action includes a small instruction blob (the payload) saying what should happen.
The covenants check the instruction blob is consistent with history and with the transaction being built.

---

## Glossary

- **Script**: a bytecode program (Kaspa txscript) that validates a spend. Covenants are scripts that _restrict_ spending.
- **ScriptPublicKey (SPK)**: the output locking data (version + script). In this create, we often serialize it as **`spk_bytes = spk.to_bytes()`**.
- **Signature script / witness**: per-input data that provides arguments to the spending script. Here it includes the **backtrace fragments**.
- **Payload**: fixed-size bytes stored in the transaction payload field. The covenants parse it at known offsets.
- **Preimage**: the bytes that are hashed to form a TransactionID (in Kaspa: the “transaction id preimage”).
- **TransactionID**: the hash of a transaction preimage (including payload). The scripts recompute parent/grandparent TransactionIDs from supplied fragments.
- **Backtrace**: a compact proof of lineage carried in the witness: fragments of the **parent** and **grandparent** (preimage + payload).
- **Outpoint**: `(txid, index)` identifying a specific UTXO.
- **Token quantity**: the logical amount of the native asset (tracked in payload + lineage). Distinct from the KAS value used to pay fees.

---

## Protocol overview

### What is “the state”?

We model the protocol as a covenant-enforced state machine. We’ll use:

- **`MinterState`** for the minter covenant’s state tuple, and
- token outputs whose “ownership + quantity” are defined by the _creating transaction’s payload_ and validated by lineage.

### Identities and state fields

The minter covenant tracks:

`MinterState = (asset_id, authority_spk_bytes, token_spk_bytes, remaining_supply)`

- `asset_id`: identifies the asset lineage (see **Asset identity** below)
- `authority_spk_bytes`: serialized SPK bytes of the mint authority script
- `token_spk_bytes`: serialized SPK bytes of the **associated token covenant script**
- `remaining_supply`: decreases on each mint **by the minted `total_amount`**

Token movements are validated by the token covenant. Token UTXOs themselves are typically _uniform covenant outputs_;
their **token quantity and recipient** are defined by the parent payload slot that created them, and enforced by the backtrace checks.

---

## Operations

Operations are identified by a single-byte **protocol operation kind** (`op`):

- `Mint = 0`
- `SplitMerge = 2`

---

## Mint transition (minter covenant)

A **Mint** spend consumes the current minter state and produces:

- a new minter covenant output committing to the next `MinterState`, and
- a token covenant output (or outputs) as specified by the payload and enforced by the script.

In this implementation, the minter covenant enforces (conceptually) the following checks:

1. **Payload format**
   - Current payload length MUST match the mint payload length.
   - Payload MUST start with `MAGIC`.
   - `op` MUST be `Mint`.

2. **Lineage + asset identity**
   - Parent payload MUST also be a mint payload with `op = Mint` (the minter “chain” only follows mint).
   - `asset_id`, `authority_spk_bytes`, and `token_spk_bytes` MUST be inherited unchanged from the parent payload.
   - On the _genesis branch_ (see **Backtrace & genesis/continuation**), `asset_id` MUST equal the anchor outpoint derived from the parent’s input0 prevout.

3. **Supply transition**
   - `total_amount >= 1`.
   - `parent_remaining_supply >= total_amount`.
   - `remaining_supply == parent_remaining_supply - total_amount`.

4. **Mint output declaration consistency**
   - `total_amount` MUST equal the payload’s declared mint output amount (`output0_amount`).
   - The declared recipient SPK bytes length MUST be within bounds.

5. **Authority binding**
   - The payload’s `authority_spk_bytes` MUST equal the authority script configured in the minter covenant.
   - An authorization input MUST spend that authority script (in this example, `input[1]`).

6. **Output commitments**
   - `output[0]` MUST loop back to the same minter covenant script.
   - `output[1]` MUST be locked to the token covenant SPK bytes carried in `token_spk_bytes`.
   - The transaction MUST have at least 2 outputs (minter + token output).

**Invariant:** supply can only decrease via the minter covenant’s `remaining_supply` rule, and minting requires authority.

---

## Split/Merge transition (token covenant)

A **SplitMerge** spend consumes one or more token covenant UTXOs and produces one or more token covenant UTXOs.
The token covenant enforces conservation of token quantities and recipient authorization based on the parent payload.

In this implementation, the token covenant enforces (conceptually) the following checks:

1. **Payload format**
   - Current payload length MUST match the transfer payload length.
   - Payload MUST start with `MAGIC`.
   - `op` MUST be `SplitMerge`.

2. **Genesis vs continuation rules**
   - **Continuation:** parent payload MUST be a transfer payload with `op = SplitMerge`.
   - **Genesis (mint-origin):** parent payload MUST be a mint payload with `op = Mint`, and the lineage MUST prove the mint is rooted in the expected minter covenant (see **Backtrace & mint-origin binding**).

3. **Inherited identity**
   - `asset_id`, `authority_spk_bytes`, and `token_spk_bytes` MUST be inherited unchanged from the parent payload.

4. **Bounded arity + “no gaps”**
   - The number of token inputs and token outputs is bounded by `MAX_INPUTS_COUNT` and `MAX_OUTPUTS_COUNT`.
   - Payload amount arrays are fixed-size with trailing zero padding; non-zero entries MUST be contiguous from index 0.
   - Recipient slots MUST be empty (length 0) for any output with amount 0.

   > In the reference implementation, `MAX_INPUTS_COUNT = 2` and `MAX_OUTPUTS_COUNT = 2`, so transfers are 1–2 inputs and 1–2 outputs.

5. **Conservation**
   - `sum(input_amounts) == sum(output_amounts)`.

6. **Input amount and owner binding to history**
   - For each token input being spent, the covenant identifies which **parent payload output slot** created it and checks:
     - the parent output amount equals the current input’s declared amount, and
     - the parent output recipient SPK bytes equals the script of the authorization input.

   The authorization input is expected to be present and, in this example, is taken as the **last input**.

7. **Output commitments**
   - All created outputs MUST be token covenant outputs (locked to the same token covenant script).

**Invariant:** token transfers can redistribute balances but cannot change the total.

---

## Backtrace, genesis/continuation, and anti-replay

Every covenant spend carries a compact **backtrace** in the witness (signature script). It includes:

- grandparent preimage (without payload),
- grandparent output0 SPK bytes,
- grandparent payload,
- parent preimage (without payload),
- parent payload.

The covenant script then:

1. **Binds the parent to the spent outpoint**
   - Recomputes `parent_txid` from `(parent_preimage || parent_payload)` and requires it equals the current input outpoint txid.

2. **Binds the grandparent to the parent’s input0 prevout**
   - Extracts the parent input0 prevout from the parent preimage.
   - Recomputes `gp_txid` from `(gp_preimage || gp_payload)` and requires it equals that prevout txid.

3. **Determines continuation vs genesis**
   - Compares `gp_output0_spk_bytes` to the current covenant SPK bytes.
   - If they match: **continuation** (the parent’s input0 spent a UTXO locked by the same covenant).
   - If they don’t: **genesis branch** for that covenant.

### Asset identity derivation (minter covenant)

On the genesis branch for the minter covenant, the script derives the anchor outpoint as:

`asset_id = parent.input0.prevout_txid || parent.input0.prevout_index_le`

and requires it equals the payload’s `asset_id`.
To keep this deterministic, genesis requires that the anchor prevout index is fixed (in this implementation: `0`).

### Mint-origin binding (token covenant)

On the genesis branch for the token covenant (i.e., when spending token outputs created by a mint payload),
the script additionally enforces that the mint is rooted in the expected **minter covenant**.
Conceptually: it proves that the mint transaction spent a UTXO whose locking script matches the configured minter covenant.

This prevents “forged mints” from being accepted as valid token origins.

### Why this matters

This backtrace design makes payloads **non-replayable and non-swappable**:

- a payload is tied to the specific parent txid and current outpoint,
- history can’t be spliced without breaking hash checks,
- token ownership and amounts are anchored to the creating payload slot and enforced on spend.

---

## Payload specification

This crate uses **two fixed-length layouts**: one for mint and one for transfer.

### Common encoding rules

- `asset_id`: 36 bytes = `txid(32) || index_le(4)`
- numbers: `u64` little-endian
- `spk_bytes`: `spk.to_bytes()` (version + script), with length constrained to a small range in this example
- SPK fields in payload are encoded as:
  - `len(1) || spk_bytes || zero padding`
- unused recipient slots in transfer payload use:
  - `len = 0` and all-zero padding

### Mint payload fields

`MAGIC || asset_id || authority_spk_bytes || token_spk_bytes || remaining_supply || op || total_amount || output0_amount || output0_recipient_spk_bytes`

### Transfer payload fields

`MAGIC || asset_id || authority_spk_bytes || token_spk_bytes || op || total_amount
 || input_amounts[MAX_INPUTS_COUNT]
 || output_amounts[MAX_OUTPUTS_COUNT]
 || output_recipients[MAX_OUTPUTS_COUNT]`

### Amount array “no gaps” rule

When decoding transfer payloads, non-zero entries must be contiguous from index 0.
Once a zero is observed, all later entries must be zero.

This keeps parsing deterministic and ties directly to the transaction’s input/output counts.

---

## Transaction shapes (what the covenants expect)

### Mint transaction shape

- **Inputs**
  - `input[0]`: minter covenant UTXO
  - `input[1]`: authority UTXO (must match the configured authority script)
- **Outputs**
  - `output[0]`: next minter covenant UTXO (loops back)
  - `output[1]`: token covenant output (script must match `token_spk_bytes`)
    (TODO: support giving back the authorization input)

### Transfer transaction shape

- **Inputs**
  - 1–`MAX_INPUTS_COUNT` token covenant inputs
  - plus 1 authorization input (expected last), whose script must match the parent recipient for each spent token input
- **Outputs**
  - 1–`MAX_OUTPUTS_COUNT` token covenant outputs
    (TODO: support giving back authorization input)

### Index mapping: minted token outputs vs transfer outputs

Ownership/amount for a token UTXO is read from the **parent payload slot** that created it.
This implementation uses a simple mapping:

- For token outputs created by a **transfer** payload, output indices line up naturally (outpoint index `i` ↔ parent output slot `i`).
- For token outputs created by a **mint** payload, the token output is commonly placed at transaction `output[1]` while the mint payload only has `output0_*`.
  The token covenant therefore maps `parent_output_index = outpoint_index - 1` for mint-origin spends.

---

## Example flows (high level)

### 1) Create the asset and mint

1. A minter covenant UTXO exists (the “current minter state”).
2. Build a mint payload:
   - inherit `asset_id`, `authority_spk_bytes`, `token_spk_bytes`
   - choose `total_amount = X`
   - set `remaining_supply = parent_remaining_supply - X`
   - set the minted recipient SPK bytes
3. Create a mint transaction:
   - spend minter state at `input[0]`
   - spend authority at `input[1]`
   - create `output[0]` as next minter state
   - create `output[1]` as token covenant output for the minted recipient

### 2) Split / merge / transfer tokens

1. Build a transfer payload:
   - list the input amounts (by token input index)
   - list the output amounts + recipients
   - enforce `sum(inputs) = sum(outputs)`
2. Create a transfer transaction:
   - spend token inputs
   - include an authorization input matching the current owner(s)
   - create 1–2 token covenant outputs as declared

---

## What this example guarantees

- **Supply control:** minting requires authority and decrements `remaining_supply` by the minted `total_amount`.
- **Conservation:** token transfers conserve total token quantity.
- **Ownership:** token spends require an auth input matching the recipient recorded in the parent payload.
- **Anti-replay / anti-splice:** backtrace binds the spend to its lineage and prevents payload swapping.

---

## Notes for developers and Kaspa contributors

- The covenants rely on being able to:
  - read fixed substrings of the payload,
  - read the spending input’s outpoint (txid + index),
  - read input/output SPKs
  - recompute TransactionIDs from provided preimage fragments.
- The reference scripts currently support small fixed maximums (see `MAX_INPUTS_COUNT`, `MAX_OUTPUTS_COUNT`) for simplicity.
