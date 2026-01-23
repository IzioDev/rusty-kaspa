# Native Asset Covenants (Minter + Token) - protocol example

This crate demonstrates a native-asset protocol implemented as two cooperating covenants:

- a minter covenant that controls supply
- a token covenant that controls ownership and conservation

State is stored inside the script public key itself (state-in-script). Script hashes are not stable because
state changes each spend. The protocol uses covenant_id and covenant outputs (OpCovOutputCount/OpCovOutputIdx)
for output authorization instead of parent/grandparent backtraces.

Transactions must use version 1 (post covenant HF) so outputs can carry `cov_out_info`.

---

## Mental model (non-technical)

- Minter = the printer. It can mint up to a capped supply.
- Token = the wallet layer. It lets holders split, merge, and transfer balances.

Each spend must create outputs whose scripts encode the next state. The covenant scripts validate that
state transition directly from output scripts.

---

## Glossary

- Script: Kaspa txscript program that validates a spend.
- ScriptPublicKey (SPK): output locking data (version + script).
- Covenant id: a stable id carried by consensus for covenant outputs; used as the asset id.
- cov_out_info: output field declaring which input authorizes the output and which covenant_id it carries.
- Authorization input: a regular input that must match the current owner/authority SPK.

---

## Asset identity

The asset id is the covenant_id, derived from the outpoint of the initial minter input
and carried forward by consensus. Every covenant output in this protocol must set:

- `cov_out_info.authorizing_input = 0`
- `cov_out_info.covenant_id = covenant_id`

This lets covenant scripts access their authorized outputs using OpCovOutputCount/OpCovOutputIdx.

---

## Script state layout

Both minter and token scripts start with a fixed prefix that encodes state:

- covenant_id: 32 bytes
- amount: 8 bytes (u64 little-endian)
  - for minter: remaining_supply
  - for token: token amount
- spk_field: 1 byte length + 37 bytes padded
  - for minter: authority_spk_bytes
  - for token: owner_spk_bytes

The remainder of the script is a fixed logic tail. Outputs are validated by checking this tail and
parsing the state prefix from output scripts.

---

## Minter transition

A minter spend consumes the current minter UTXO and produces exactly two covenant outputs
(authorized by input 0):

- output 0: next minter state
- output 1: token output

Checks enforced by the minter script:

1) Input index is 0, and there is an authority input at index 1.
2) Authority input SPK matches the authority_spk_bytes in the state prefix.
3) Output count for input 0 is exactly 2.
4) Output 0 uses the same minter logic tail and carries the same authority field.
5) Output 1 uses the token logic tail and has a valid owner SPK length.
6) minted_amount > 0
7) remaining_supply >= minted_amount
8) output0.remaining_supply == remaining_supply - minted_amount

Recipient selection for the token output is defined by the output script itself.

---

## Token split/merge transition

A token spend consumes one or two token inputs and produces one or two token outputs.
The authorization input is always the last input.

Checks enforced by the token script:

1) Input count is 2..=3 (token inputs + auth input).
2) current input index < auth input index.
3) Authorization input SPK matches owner_spk_bytes in the state prefix.
4) If two token inputs are present, their covenant_id fields must match.
5) Output count for input 0 is 1..=2.
6) All authorized outputs use the token logic tail and have valid owner SPK lengths.
7) All input and output amounts are positive.
8) sum(input amounts) == sum(output amounts).

---

## Why covenant_id

Because state is inside the script, script hashes change each spend. The protocol relies on
covenant_id (carried by consensus and declared in cov_out_info) for identifying covenant outputs.

---

## Implementation notes

- Transactions must be version 1 (post covenant HF).
- Every covenant output must set cov_out_info with authorizing_input = 0.
- The VM example builds a covenants context so OpCovOutputCount/OpCovOutputIdx work locally.
