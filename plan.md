# KCC-3 account implementation plan

Status: The current implementation phase covers `kcc3` and `kcc3-watch` P2PK accounts only. The architecture proposal below also describes future work; its multisig sections are deferred. Existing multisig implementations receive only the mechanical fallible-derivation signature adaptation required by the shared trait.

## Implemented P2PK phase (2026-10-20)

- Added validated purpose-44 origins, all four assigned network subtrees, Schnorr/ECDSA separation, strict account-xpub imports, and complete public/private address paths in wallet-keys. The pinned P2PK vectors and independent public/private checks pass; general BIP32 and gen0/gen1 derivation remain unchanged.
- Registered `kaspa-kcc3-standard` (`kcc3`) and `kaspa-kcc3-watch-standard` (`kcc3-watch`). Both use the KCC-3 payload and an independent versioned identity encoding. Fingerprint annotations, counters, names, and attached network contexts do not change identity.
- Added version-one per-network progress metadata while preserving version-zero bytes for existing account metadata. Encrypted wallet backups retain both the public context payloads and progress maps.
- Added asynchronous network-context transitions, missing-context availability descriptors, explicit seed/watch context provisioning, immutable runtime views, and generation checks. Testnet instances share keys/progress and receive fresh chain state. Returning from an unavailable destination restores the previous account and counters.
- Added scheme-aware P2PK signing and unsigned watch bundles, complete input/change origins, scheme-aware bundle signatures, and real txscript-engine verification. Commit/reveal explicitly rejects KCC-3 in this phase.
- Added independent Schnorr/ECDSA UTXO discovery with the existing extents/stopping behavior. Added creation/import defaults, explicit legacy choices, typed KCC-3 mnemonic import, CLI watch import and mnemonic export with plain xpub and scheme exchange, network setup prompts, and WASM creation/import/context conversion.

### API and format notes

`DerivationCapableAccount::derivation()` now returns `Result<Arc<dyn AddressDerivationManagerTrait>>`. Native live-wallet callers use `Wallet::change_network_context`; the synchronous setter rejects cross-network changes while bound views or remembered account activations exist. WASM `setNetworkId` now returns a Promise. `GeneratorSettings` has an optional `context_account` field to retain context validation through generation/submission; standalone callers set it to `None`.

The new `AccountCreateArgs` variants follow the five existing Borsh variants. The native/WASM public description retains `version: 1`, `convention: "kcc3"`, `kind`, `network`, `signatureScheme`, `accountIndex`, and `key` (`origin`, optional `masterFingerprint`, `xpub`). Redundant origin fields must agree. Public descriptions never include local key-data IDs or secrets.

**User-confirmed CLI simplification:** `account watch kcc3` asks for an extended public key, then its signature scheme (Schnorr by default); it does not require JSON. The current wallet network supplies the network, and the hardened account index comes from the xpub. The unknown master fingerprint stays absent. Export supports only `export mnemonic`, which includes the xpub, scheme, network, and account index alongside the mnemonic. `account import mnemonic` requires an explicit type (`kcc3`, `bip32`, `legacy`, or `multisig`) and prints usage when the type is missing. KCC-3 mnemonic import asks for the mnemonic and then the scheme, retaining the existing wallet/password and optional BIP39 passphrase prompts. Mnemonic import continues to restore account zero. When selecting a network while disconnected, `network <id>` asks for the destination xpub and scheme for watch accounts missing that context; an empty xpub skips setup. Cached contexts require no prompts. This supersedes the original JSON recommendation below for the CLI P2PK flow.

WASM exposes `provisionKcc3Context`; native callers use `Kcc3::provision_context`. Native mnemonic import uses generic `AccountImportArgs` through `Wallet::import_mnemonic`; `Wallet::import_with_mnemonic` retains the existing kind-based entry point. Default-account creation and new-wallet initial account creation accept only `kcc3` and `bip32`; legacy remains available through its existing explicit import/create paths.

**User-confirmed invalid-child policy:** reject every invalid BIP32 child, including automatic account/address allocation and discovery. Propagate the error without retrying, skipping an index, or changing the requested origin. Failed address allocation leaves the saved counters unchanged; a failed range does not populate its address cache. This matches the existing BIP32/gen0/gen1 behavior. Tests inject scalar overflow and zero/infinity child results and a failed address derivation. The draft's requirement for BIP32 invalid-child handling still needs clarification before claiming conformance for this edge case; ordinary vectors do not exercise it.

### Function ownership audit

New core functions are inherent methods, required trait implementations, or test-only functions. The CLI watch wizard is the user-requested module-level I/O boundary. The following groups enumerate their owners and the invariant justifying each operation; orchestration remains in the existing callers.

| Owner and functions | Current consumers / owned boundary |
| --- | --- |
| `AccountDerivation::{try_new, network, signature_scheme, account_index, account_path, address_path, from_address_path, derive_account, derive_private_key}` | Creation, imports, public derivation, signing, discovery and origin export; owns registries, hardening/depth and index bounds. Accessors preserve private validated fields. |
| `AccountDerivation::child_error` | CKD/private/public error translation; owns invalid-child classification and propagation without retry. |
| `AccountPublicKey::{try_new, derivation, xpub, master_fingerprint, derive_public_key}` | Watch imports, address managers, range scans, PSKT metadata and allocation; owns origin/xpub depth/child consistency and exact compressed CKD. Invalid children are rejected. |
| `AccountDerivation` / `AccountPublicKey` conversion and Borsh trait methods (`try_from`, `from`, `deserialize_reader`) | Storage and JSON exchange; validate every deserialization and encode canonical origins. |
| `Ctor` factory trait methods (`name`, `description`, `try_load`) | Two account-kind registrations; existing factory contract. |
| `PublicAccountDescription::{try_from_xpub, try_key}` | Watch creation and native/CLI/WASM context import; reconstructs the public description from a selected network/scheme and an account xpub, validates depth/hardening, and owns version/convention/kind and origin consistency. The constructor is public for CLI use. |
| `Payload::{try_load, identity}` and Borsh methods (`serialize`, `deserialize_reader`) | Account factory, allocation, provisioning and creation; owns versioned payload/identity encoding and context consistency. |
| `Kcc3::{try_new, try_load, active_key, public_description, key_source, annotate_bundle}` | Factories, creation/import, public export, direct signing and bundles; owns immutable runtime construction, active origin and complete PSKT input/change metadata. `annotate_bundle` has two production callers. |
| `Kcc3` Account trait methods (`inner`, `account_kind`, `as_dyn_arc`, `xpub_keys`, `prv_key_data_id`, `ensure_active_context`, `balance`, `sig_op_count`, `minimum_signatures`, `receive_address`, `change_address`, `default_address`, `account_addresses`, `to_storage`, `metadata`, `descriptor`, `as_derivation_capable`, `create_signer`, `pskb_from_send_generator`, `pskb_sign`) | Existing account callers; implements the lifecycle, persistence, signing and watch-capability contract on its actual owner. |
| `Kcc3` DerivationCapableAccount methods (`derivation`, `account_index`, `create_private_keys`) and `Drop::drop` | Existing derivation/signing callers and runtime disposal; fallible manager availability, seed/xpub verification and balanced live-view accounting. Other account implementations only adapt `derivation` to return `Ok`. |
| `Account::{ensure_active_context, create_signer}` | Shared address/scan/generation/send/sweep/transfer callers; default trait contracts preserve existing account behavior and allow the KCC-3 override. |
| `AddressManager::ensure_active_context`, `AddressDerivationManager::new_kcc3` | Address operations and account/discovery constructors; binds prefixes, managers and private paths to a validated immutable origin. |
| `Kcc3PubkeyManager` trait methods (`new_pubkey`, `current_pubkey`, `index`, `set_index`, `get_range`) | Existing public-key manager interface; owns one validated public chain and its index. |
| `PrvKeyData::create_kcc3_xpub` | Creation, typed import, provisioning and discovery; owns credential/seed access and actual master-fingerprint derivation. |
| `AccountMetadata::{new_kcc3, kcc3_indexes, set_kcc3_indexes}` | Account loading, allocation, scanning and switching; owns independent bounded progress maps. |
| `Kcc3Signer::new`, `SignerT::try_sign` | Account signer construction and direct generation; owns context-bound secret lifetime, input/script matching, sighash selection and signature encoding. |
| `PSKBSigner::sign_ecdsa`, `PSKBSignerInner::drop`, `PSKTGenerator::new_unsigned`, adapted `PSKTStream::new` | Bundle signing, disposal and watch construction; owns ECDSA signing, cached-key zeroization and secret-free generation. |
| `Generator::context_account` | Generation and pending submission; retains the immutable account view at the runtime boundary. |
| `Wallet::{context_guard, context_generation, retain_context_view, release_context_view, apply_network_id, change_network_context}` | Account lifecycle, pending submission and native/API/CLI/WASM network setters; owns locking, view lifetime, RPC network selection and coherent transitions. `apply_network_id` has two production callers. |
| `Kcc3::{try_new_from_prv_key_data, next_account_index, provision_context, scan_accounts}` | Construction from creation/import arguments (two callers), seed/subtree allocation (two callers), CLI/WASM context attachment and discovery API; owns origin validation, duplicate allocation, payload context consistency and the KCC-3 UTXO subtree scan. Constructors and allocation stay on the account owner; provisioning and discovery isolate storage/RPC boundaries. |
| `AccountImportArgs::try_create_account` and `TryFrom<AccountKind>::try_from` | Shared construction for mnemonic import and initial wallet creation (two callers); owns supported-kind conversion/defaults and dispatch to account constructors. The typed enum carries explicit KCC-3 origins without adding account-specific parameters to `Wallet`. |
| `Wallet::import_mnemonic` | Kind-based wrapper and explicit CLI import; owns verified seed reuse, wallet locking, shared persistence and account startup. The existing `import_with_mnemonic` signature remains supported. |
| CLI `wizards::account::kcc3_watch` | One command-dispatch caller; user-requested placement in the existing account-wizard module. Owns watch-import/password and account-creation I/O, matching adjacent watch wizards. |
| CLI `wizards::import::prompt_for_kcc3_public_account` | Two callers: watch import and destination-network setup. Owns the shared two-step xpub/scheme terminal I/O boundary; empty input lets each caller abort or skip. This free function belongs beside the existing mnemonic prompt and has no account instance to own it. |
| Test-only `public_xpub_import_preserves_account_origin` | Covers all four networks, both schemes, zero/nonzero/maximum account indices, xpub/kpub/ktub encodings, receive/change private-key agreement, and invalid depth/hardening rejection. Existing watch tests also exercise bare-xpub address derivation and unsigned-bundle signing without a master fingerprint. |
| WASM Wallet `{set_network_id, provision_kcc3_context}` | JS consumers; async lifecycle and DTO/credential conversion boundary. |
| Test-only `Fixture::new`; KCC-3 tests (`network_contexts_counters_and_stale_handles`, `watch_ranges_identity_and_serialization`, `watch_unsigned_bundle_and_both_scheme_signatures`, `backup_coexistence_allocation_and_missing_context_return`, `discovery_subtrees_keep_utxo_extents_independent`, `watch_context_provisioning_and_default_creation`, `p2pk_signatures_execute_and_reject_substituted_utxos`); key tests (`invalid_children_are_rejected`, `p2pk_conformance_vectors`, `all_networks_schemes_chains_and_import_boundaries`); metadata test `legacy_bytes_and_network_progress` | Test-owned fixture and behavioral/compatibility acceptance checks. The seven runtime tests also run in Node/WASM. |
| Test-only `FailingPubkeyManager` trait methods (`new_pubkey`, `current_pubkey`, `index`, `set_index`, `get_range`) and `kcc3_invalid_child_does_not_skip_or_advance` | Injected CKD failure at the existing public-key-manager boundary; verifies exact-index rejection, no retry, unchanged counters and no partial range cache. No production helper was added. |

### Validation for this phase

Earlier phase validation (before the subsequent user-confirmed rejection-policy change): `cargo test -p kaspa-wallet-keys -p kaspa-wallet-core -p kaspa-wallet-pskt` (78 native tests plus doc-test targets), `cargo check -p kaspa-wallet-core --features wasm32-sdk --target wasm32-unknown-unknown`, and `wasm-pack test --node wallet/core --features wasm32-sdk` (12 tests). Native CLI/core/keys all-target Clippy with the WASM SDK feature and `-D warnings` also passed. Final formatting/diff checks and the final post-review reruns are recorded in the implementation handoff.

The entire workspace release matrix below has not been run: validation was scoped to the affected wallet crates, CLI and actual WASM runtime. No live funded-network submission is claimed; transaction correctness uses synthetic UTXOs and the real script engine.

## Original architecture proposal (future multisig work deferred)

Specification: [KCC-0003](../kccs/kcc-0003.md), draft dated 2026-10-20, and its [conformance vectors](../kccs/kcc-0003/vectors/conformance.json). Review and pin the agreed specification/vector revision before implementation because the KCC is still a draft.

## Goal and account model

Add KCC-3 accounts alongside the existing account types, preserving existing derivation, addresses, storage identities, and spending behavior.

| Account type | Responsibility |
| --- | --- |
| `kcc3` | Seed-backed P2PK account using purpose `44`, with an explicit network, signature scheme, and account index. |
| `kcc3-watch` | Watch-only P2PK account deriving receiving and change addresses from a KCC-3 account xpub and its origin metadata. |
| `kcc3-multisig` | Policy-backed multisig account using purpose `45`, with cosigner account xpubs, origins, signature schemes, and optional local signing keys. |

**Confirmed: keep `kcc3-multisig` as a distinct account kind.** It owns a different spending policy and backup contract from P2PK, and its derivation and sorting rules differ from today's `multisig`. KCC-3 itself does not require these application-level account types; this choice follows the repository's existing account/factory model.

A `kcc3-multisig` account with no local signing keys provides watch-only multisig behavior. A fourth account kind is not needed. The final Rust type layout and persisted kind identifiers remain implementation details to refine.

Other confirmed decisions:

- Support homogeneous Schnorr and homogeneous ECDSA multisig in this implementation. Mixed-scheme policies are out of scope and must fail explicitly on import or use.
- Make `kcc3` the default account kind for new wallet/account creation and mnemonic imports when no kind is selected. Preserve an explicit kind choice during import so existing `bip32`, `legacy`, and `multisig` accounts remain recoverable using their original conventions.
- Preserve account kinds recorded in existing wallet files and typed backups. Changing the default must not convert already stored accounts or override a backup's declared convention.
- Keep the existing UTXO-based recovery/discovery method and adapt it for KCC-3 account derivation. Preserve existing scan limits, windowing, and stopping behavior within each subtree; no recovery algorithm redesign or historical-activity backend is in scope.

## Required behavior

- Derive exactly `m / purpose' / coin_type' / sig_scheme' / account' / change / address_index`. Harden the first four levels; keep the last two non-hardened. There is no additional cosigner level.
- Support the assigned networks: mainnet `111111`, testnet `111112`, simnet `111113`, and devnet `111114`. Testnet instances share the testnet coin type; connection/network-instance settings remain separate from derivation.
- Use scheme `0` for Schnorr and `1` for ECDSA. Keep compressed public keys throughout BIP32 derivation; encode final keys as x-only Schnorr keys or compressed ECDSA keys.
- Reject unassigned registry values, invalid hardening/depth, change values other than `0` or `1`, and components outside `0..=2^31-1`. Preserve BIP32 invalid-key handling. Apply KCC-3 validation at its own boundaries without restricting general BIP32 or legacy paths.
- Treat network and scheme as explicit derivation choices. For seed-backed KCC-3 accounts, switching networks should select the corresponding network subtree, with separately retained state, as proposed below. An existing network's keys and state must remain recoverable when switching back.
- Allocate account indices independently within each seed's `(purpose, coin_type, sig_scheme)` subtree, normally sequentially. Multisig cosigners must agree on the account index and network, including when their schemes differ.
- For every multisig address, derive the same `change / address_index` suffix from each cosigner account xpub, encode the resulting keys, and sort those encodings lexicographically. Retain each key's scheme and signer association through sorting, script construction, and signing.
- Retain multisig threshold, cosigner account keys and origins, schemes, and deterministic script construction as policy metadata. KCC-3 specifies derivation and ordering, not interoperable redeem-script bytes or a complete policy format.
- Recover each supported subtree independently. An empty subtree must not end another subtree's search. Retain or request account-allocation and gap-limit settings; multisig recovery additionally requires its policy.

## Network switching recommendation

Agree with switching derivation when the user switches networks. For example, a Schnorr P2PK account moving from mainnet to testnet selects `m/44'/111112'/0'/account'` instead of `m/44'/111111'/0'/account'`, then derives its receiving/change suffixes there.

- Treat this as selecting another network context for the logical account. Keep each context's account xpub, address progress, derivation caches, and allocation information. Scope balances, subscriptions, and transaction state to the actual network instance. Switching back restores the prior context rather than resetting it.
- Testnet instances share KCC-3 keys because they share a coin type, but their UTXO and transaction state must remain separate.
- A seed-backed account can derive a missing context when its seed is accessible. Reuse an existing context without requiring seed access; define the unlock flow for a missing context instead of silently reusing the previous network's keys.
- A watch-only account cannot derive another hardened network branch from its current account xpub. Require the destination network's xpub and origin before making that context available.
- Multisig likewise requires a complete destination-network policy and all cosigners' destination account xpubs. Local seed access can provide local cosigner keys, but cannot manufacture remote cosigners' branches. Keep the account unavailable on that network until the required policy is present.
- Coordinate context selection, subscription/cache replacement, and account activation with network switching. The current network setter only permits switching while disconnected; changing that restriction is not required by this plan.

The Rust proposal below gives the logical account a stable identity and saves its network contexts separately. The intended user behavior is network switching with the correct subtree, preserving previously saved network contexts.

## Import/export recommendation

No additional files or input are needed from the user at this stage. The remaining design work is defining what accompanies an account xpub and multisig policy during exchange.

- Keep the existing encrypted full-wallet backup mechanism and include the new account/context metadata in it.
- Provide a versioned public account description, preferably JSON, for watch-only and multisig exchange: account kind/convention, network, scheme, account index, account xpub, and origin. Include the threshold, cosigner origins/schemes, and script-template identifier for multisig. Exclude private keys and mnemonics from this public description.
- Retain address progress and recovery settings in wallet backups; decide which are also useful in public exports. An exported account xpub is for a particular hardened network/scheme/account branch.
- For raw mnemonic import, offer an explicit account-kind choice with `kcc3` selected by default. Keep legacy import paths selectable; do not infer the derivation convention solely from mnemonic length. Preserve the convention declared in typed account exports/backups.
- Validate structural consistency and supported policies on import. A bare xpub cannot prove its hardened ancestry. The exact schema and any external-wallet interoperability target can be settled in the detailed plan; an external target is not required to proceed with repository-native support.

## Recovery today and confirmed scope

Current behavior in this checkout:

- `scan_bip44_accounts` searches only existing BIP32 P2PK Schnorr accounts on the connected network. It derives both receiving and change addresses over the caller's `address_scan_extent`, advances account indices according to `account_scan_extent`, and extends the search when it finds an account with UTXOs. It reports the last funded account index.
- Ordinary account scanning checks both address chains using current UTXOs, with a default window size of `8`, taking the saved address index into account. The generic API account-import path requests windows of `100` and scan depth `5000`; those values are specific to that path rather than universal recovery defaults.
- These flows call `get_utxos_by_addresses`. They do not discover historical use of fully spent addresses/accounts, and the account-discovery API currently exposes only `bip44`. Stored wallet metadata helps retain known address progress; a seed-only recovery lacks that information.

Confirmed scope: retain the existing UTXO-based method and adapt its derivation/account-kind selection for KCC-3 on the selected network. Reuse existing scan parameters, windowing, stopping behavior, and saved address progress. Apply the same method independently to each supported KCC-3 subtree so an empty subtree does not stop another. Multisig scanning requires its retained policy; watch-only scanning uses the supplied account xpub. Existing legacy discovery behavior remains unchanged. The current limitation for spent-only accounts remains; historical-activity discovery and new recovery backends are out of scope.

## Rust architecture proposal

### 1. Boundaries and type ownership

Keep general BIP32 in `wallet/bip32`, KCC-3 derivation rules in `wallet/keys`, and account policy/lifecycle in `wallet/core`. Do not modify gen0/gen1 derivation defaults or introduce a replacement wallet framework.

| Proposed owner | Types / responsibility | Why this owner |
| --- | --- | --- |
| `wallet/keys/src/derivation/gen2/hd.rs` | `Purpose`, `SignatureScheme`, `AccountDerivation`, `AccountPublicKey` | Owns the KCC-3 path, registry, account-xpub, and public/private derivation invariants. Public only where wallet-core or key-generation callers require it. |
| `wallet/core/src/account/variants/kcc3/mod.rs` | `Kcc3Account`, factory, kind constants, payload, saved network contexts | Owns account lifecycle, key-data associations, immutable identity, and active-context selection. |
| `wallet/core/src/account/variants/kcc3/policy.rs` | `MultisigPolicy`, cosigner descriptions, `ScriptTemplate` | Owns threshold, homogeneous schemes, canonical policy encoding, per-address ordering, and redeem-script construction. Used by addresses and signing. |
| `wallet/core/src/derivation.rs` | Existing `AddressManager` / `AddressDerivationManager` | Reuses chain indices, address lookup maps, and scan-facing managers, adding explicit KCC-3 construction behavior. |
| `wallet/core/src/storage/metadata.rs` | Versioned per-network address progress | Owns persistence of counters without decrypting/re-encrypting account payloads for every new address. |
| `wallet/core/src/tx/generator/signer.rs` | `Kcc3Signer: SignerT` | Owns scheme/policy-aware signing with an immutable context and ephemeral local key material. Existing signer behavior remains available for legacy kinds. |
| Existing `impl Wallet`, `WalletApi`, CLI/WASM modules | Creation, import, network orchestration, discovery, DTO conversion | Existing callers already own serialization, wallet locking, persistence, and user interaction. Keep orchestration there. |

Three public account kinds do not require three copies of the account implementation. Propose one concrete `Kcc3Account` with validated P2PK/multisig policy and key-data associations, registered under:

| Constant | Persisted kind | Accepted short name |
| --- | --- | --- |
| `KCC3_ACCOUNT_KIND` | `kaspa-kcc3-standard` | `kcc3` |
| `KCC3_WATCH_ACCOUNT_KIND` | `kaspa-kcc3-watch-standard` | `kcc3-watch` |
| `KCC3_MULTISIG_ACCOUNT_KIND` | `kaspa-kcc3-multisig-standard` | `kcc3-multisig` |

The factory validates kind, payload policy, and associations together: P2PK with one local key-data reference, P2PK watch with none, multisig with zero or more verified local cosigner bindings. A zero-key multisig keeps the multisig kind. Preserve existing aliases and factory registrations.

### 2. Validated derivation and key origins

Proposed value model, showing domain fields rather than final serialization/layout:

```rust
// wallet/keys/src/derivation/gen2/hd.rs
#[repr(u32)]
pub enum Purpose { P2pk = 44, Multisig = 45 }
#[repr(u32)]
pub enum SignatureScheme { Schnorr = 0, Ecdsa = 1 }

pub struct AccountDerivation {
    purpose: Purpose,
    network: NetworkType,
    signature_scheme: SignatureScheme,
    account_index: u32,                          // validated below 2^31
}

pub struct AccountPublicKey {
    derivation: AccountDerivation,
    master_fingerprint: Option<KeyFingerprint>,
    xpub: ExtendedPublicKey<secp256k1::PublicKey>,
}
```

- `AccountDerivation` owns registry conversion, account-path construction, complete address-path validation, and derivation from a master xprv. Fields stay private so constructors and import conversions enforce bounds. Wire/storage DTOs must pass through these checks rather than unchecked derived deserialization.
- Accept account origins at depth four and complete address paths at depth six at their respective boundaries. A supplied account xpub must have matching depth and hardened account child number. Its remaining ancestry is a claim unless checked against a known seed; a master fingerprint is identification metadata, not proof.
- Use full binary extended-key data, including chain code, when comparing keys. Prefix strings and public-key bytes alone do not identify a derivation context. Require a real public extended key, not an xprv passed through a permissive parser.
- For locally derived keys, compute the master fingerprint from the master public key. Do not substitute the account node's parent fingerprint. Unknown imported master fingerprints remain absent; never invent one for PSKT metadata.
- Keep `PrvKeyData::create_xpub(...)` and the legacy `build_derivate_path(s)` helpers unchanged for their existing callers. Add an inherent KCC-3 xpub method accepting `&AccountDerivation`; KCC-3 signing overrides use the same derivation owner rather than adding another account-kind-only path switch.
- Audit both `DerivationCapableAccount::create_private_keys` and `AddressDerivationManager::get_range_with_keys_impl`: the latter currently invokes the legacy free helper directly. KCC-3 managers must carry the validated active origin and derive private suffixes through that owner as well. Multisig signing uses all requested local bindings; a one-private-key-per-address range API must not select an arbitrary cosigner. Adapt the account-owned sweep path or reject an unsupported low-level operation explicitly.
- Existing gen1 `from_extended_public_key(xpub, None)` can derive `/change/index` from a depth-four node without using its old master path. Reuse this suffix machinery only after its validation/invalid-child behavior is shown to satisfy KCC-3. Never call gen1's master derivation for KCC-3, and never supply its cosigner suffix.

**Original conformance prerequisite (current-phase decision: reject every invalid child, as recorded above):** `wallet/bip32/src/xprivate_key.rs::derive_child` explicitly returns an error instead of performing BIP32's invalid-child retry. A new path formatter alone therefore does not establish full KCC-3 conformance. Resolve compliant invalid-child behavior at the KCC-3 derivation boundary, with deterministic fault-injection tests and consistent public/private results. Do not silently alter gen0/gen1 behavior. Any retry that changes a fixed registry component, chain meaning, or agreed multisig suffix needs explicit resolution against the specification before claiming conformance; ordinary vectors do not exercise this edge case.

### 3. Stable account identity and saved network contexts

Recommend one persisted logical account and a stable `AccountId` across network switches. A runtime `Kcc3Account` is an immutable view of one selected network context; rebuilding that view avoids changing the existing borrowed `Account::xpub_keys()` interface into a lock-backed reference.

| Data | Proposed placement and invariant |
| --- | --- |
| Immutable identity anchor | Initial validated public account description, or canonical initial multisig policy. Stored in the KCC-3 payload; never replaced when another network is added. |
| Saved contexts | KCC-3 payload map keyed by `NetworkType`, each containing its validated origin/account xpub or complete multisig policy. Only public key/policy metadata is cached here. |
| Local signing associations | Existing `AccountStorage.prv_key_data_ids` plus KCC-3 cosigner-to-key-data bindings. The stored seed remains in existing encrypted key-data storage. |
| Address counters | Per-`NetworkType` map in versioned `AccountMetadata`. Testnet instances share this progress because they share derived keys; do not allocate the same addresses independently per testnet instance. |
| Active chain state | Runtime captures full `NetworkId` and a wallet context generation. Fresh `Inner`/`UtxoContext`, subscriptions, and balances for the selected instance. Reconstruct balances by the existing scan; do not display a previous network's cached balance as current. |
| Transaction records | Reuse existing storage partitioning by binding plus full `NetworkId` in both filesystem and IndexedDB stores. No transaction-store layout change is needed for this distinction. |

Derive the KCC-3 account/storage hashes from an explicitly versioned canonical encoding of kind plus immutable anchor. Include origin, full xpub data, and, for multisig, threshold and script-template identifier. Canonicalize cosigner descriptions so import list order does not change policy identity. Exclude name, address counters, runtime network, newly attached contexts, optional fingerprint annotations, and local signer availability. Adding a local key to a multisig must not rename the account.

`deterministic.rs` marks its old hash-input layout as immutable. Introduce a separate KCC-3 encoding owned by the KCC-3 payload, using the existing hashing mechanism without changing that layout or any old account hashes. This is a serialization/domain boundary, not a generic hash-helper refactor.

Allocate a fresh context's account index within its own seed/purpose/network/scheme subtree under the existing wallet guard. Read KCC-3 origins rather than counting every account of a kind. Context indices need not equal the anchor's index: persist the chosen origin and expose the active index in descriptors. Reject duplicate seed/origin bindings rather than silently attaching a different logical account. Multisig uses the explicitly agreed policy index; do not choose each local cosigner's index independently.

### 4. Versioned persistence and metadata

- Add one KCC-3 payload format with its own storage magic/version, shared by the three registered kinds. Its validated policy enum distinguishes P2PK and multisig; the outer kind/associations distinguish seed-backed P2PK from watch-only. Keep `AccountStorage`'s envelope and existing payload encodings unchanged.
- Add a versioned extension to `AccountMetadata` for `NetworkType -> AddressDerivationMeta`. Keep version-zero reads and the existing `new` / `address_derivation_indexes` behavior for legacy accounts. Write the existing version-zero form for legacy metadata, and the new form for KCC-3 metadata. Do not make the legacy `indexes` field an ambiguous mirror of whichever KCC-3 network was last active.
- Give `AccountMetadata` inherent methods for selecting/updating a KCC-3 network's counters. `Kcc3Account::metadata()` merges the active counters into the complete saved map; switching networks or updating one chain must preserve other entries. Persist that map through the existing `AccountStore::update_metadata` path, including the ordinary new-address and scan-update callers.
- Validate bounded indices and chain values again when loading metadata. A missing entry means a fresh context's initial counters, not permission to reuse another network's indices. Do not reset an existing entry during reload/import.
- Full-wallet export/import uses the existing encrypted payload and plaintext progress metadata. Keep public xpubs/policy in the encrypted payload, consistent with existing account storage, rather than moving them into plaintext metadata for convenience.
- New code reads old wallets; older code may reject KCC-3 kinds or the new metadata version explicitly. Backward readability does not imply that older binaries can load a newly created KCC-3 wallet. Test both version-zero byte stability and new-format round trips. This metadata version change is required for network switching, not an unrelated migration.

### 5. Network switching and stale runtime handles

Keep network changes on `Wallet`, using its asynchronous lifecycle and guard. The current synchronous `set_network_id` cannot safely perform storage I/O, account deactivation, or seed access.

1. Under the existing wallet guard, require the disconnected state, remember active/selected account IDs, and save the old KCC-3 counters through the existing metadata store.
2. Resolve destination contexts. Cached public contexts require no seed. Creating a missing seed-backed context requires explicit wallet/payment credentials through a secret-bearing operation; derive and persist only its public description. A bare network-selection request must not retain or guess credentials. Missing watch/multisig contexts remain unavailable until imported/provisioned.
3. Stop old runtime accounts, abort their in-flight estimation/signing/submission work, and clear old processor registrations before selecting the new full `NetworkId`. Increment a wallet-owned context generation and rebuild available KCC-3 runtime views with fresh UTXO contexts. Reuse existing reset/reload/activation pieces without recursively acquiring the wallet guard; `deactivate_accounts` already takes that guard internally.
4. Restore selection by stable account ID, emit refreshed descriptors/activation state, and use the normal scan on reconnect. An account with no destination context is still enumerable with its name, kind, and availability reason; it has no destination receiving address or balance and cannot be activated for scanning/spending.
5. A failed transition must not leave old derivation paired with a new prefix. Return to a coherent disconnected state, restoring the old selection where possible or leaving affected accounts explicitly inactive. Failure after adding a valid public context may retain that context, but must not erase old context metadata.

Route `change_network_id_call`, `connect_call`'s network selection, CLI `network`, and the WASM wallet setter through the same account-aware transition. Constructor/settings initialization can retain a low-level synchronous path before accounts exist. The existing public synchronous setter must reject a cross-network change when live KCC-3 runtime views exist, directing callers to the async path; preserving a setter that silently bypasses the lifecycle would be incorrect. The WASM live-wallet setter will consequently need an async/Promise contract; record this API change explicitly.

Add `Account::ensure_active_context() -> Result<()>` with an unchanged-behavior default for existing kinds. The KCC-3 override verifies captured full network and generation before mutable/network-sensitive operations. Check it at address allocation, scanning, key derivation, signer creation, transaction generation, and submission boundaries. Bind KCC-3 address managers to the captured network prefix rather than consulting the wallet's mutable prefix.

Keep network-sensitive operations and transition serialization coordinated through existing wallet locks; recheck context after asynchronous work before committing/submitting. A generation check only at method entry is insufficient. Old `Arc<dyn Account>` or manager handles must not save a stale payload, advance old counters, sign for the new network, or remove a replacement runtime from `active_accounts`. In particular, account stop/disconnect removal must verify that the runtime being removed is still the installed one.

**Necessary trait adjustment proposed:** change `DerivationCapableAccount::derivation()` to return `Result<Arc<dyn AddressDerivationManagerTrait>>`, because missing or stale KCC-3 contexts have no valid manager to return. Update existing implementations to return `Ok(...)` and propagate errors at callers. This is a targeted Rust source-API change with a concrete error-handling reason; retain `Account::xpub_keys()` and the other infallible borrowed metadata interfaces by using immutable runtime views. Descriptor generation must handle unavailable contexts without failing enumeration of the entire wallet.

### 6. Address construction and multisig policy

- Introduce an internal address-construction mode in the existing derivation owner, selected by explicit KCC-3 constructors. Preserve the existing constructor and legacy behavior. KCC-3 P2PK uses its explicit scheme; KCC-3 multisig uses its validated policy even for a 1-of-1 policy, rather than switching to P2PK based on the number of keys.
- `MultisigPolicy` owns `minimum_signatures`, cosigner account keys/origins, and an explicit `ScriptTemplate`. Propose identifiers `kaspa-checkmultisig-schnorr-v1` and `kaspa-checkmultisig-ecdsa-v1`, backed by the existing txscript script builders. These identifiers describe this wallet's chosen script bytes; they are not KCC-3 registry assignments.
- Its constructor checks nonempty cosigners, `1 <= threshold <= count`, same network/account index, purpose `45`, homogeneous schemes matching the template, valid account xpubs, and script/sigop limits. Reject duplicate cosigner keys; reject duplicate final encoded keys rather than representing duplicate keys as independent signers.
- Local key bindings refer to the full cosigner account key/origin, never an xpub-list position or the old `cosigner_index`. Verify local seeds derive the advertised account xpub before accepting a binding. Keep optional fingerprint annotations associated with the corresponding key.
- Derive every cosigner at the same suffix, encode according to the agreed scheme, and sort the encoded bytes. Return the ordered keys together with their cosigner associations and redeem script. That result is used both for address hashing and signature placement; recomputing it must produce identical bytes.
- Construct the redeem script with the existing Schnorr or ECDSA builder, then the existing P2SH/address encoding. Do not add consensus opcodes or mixed-scheme templates.
- Check address/range bounds before changing counters or caches. Allocating beyond `2^31-1` must fail without advancing the saved index; use checked arithmetic for incrementing indices and forming scan ranges. Preserve the existing scan strategy while making the new KCC-3 boundary obey its permitted index range.
- Fix or bypass the existing multisig range-collection issue for the new mode: `AddressManager::get_range_with_args` currently indexes a returned chunk by the absolute address index. KCC-3 must collect by chunk offset, retain the complete cosigner set for every index, and fail on an incomplete result. Cover nonzero ranges, including the scanner's second window; do not silently skip keys or addresses.

### 7. Signing, transaction generation, and partial bundles

The current generic `Signer` calls `sign_with_multiple_v2`, which computes Schnorr P2PK signatures. Wallet `PSKBSigner` also calculates Schnorr hashes, and the generic finalizer iterates the partial-signature map rather than a reconstructed KCC-3 script order. These paths cannot advertise ECDSA/multisig support unchanged.

- Add an account-owned signer construction hook used by the existing `send` and `sweep` methods. The default implementation retains their current key loading and `Signer`; `Kcc3Account` constructs `Kcc3Signer`. This removes the one-key assumption at the actual caller rather than making a multisig account pretend to implement `prv_key_data_id()` with an arbitrary first key.
- `Kcc3Signer` owns validated input/policy snapshots, local signing keys, and context checks. Use Schnorr or ECDSA sighash/signature routines according to the account policy. For each input, reconstruct the expected public key/redeem script and verify the UTXO script matches before signing. Never trust imported derivation metadata alone to authorize a key.
- Reuse `SignerT`, transaction generation, existing PSKT `Signature::{Schnorr,ECDSA}`, `KeySource`, and `Input::{redeem_script,bip32_derivations,partial_sigs}`. Account policy stays in wallet-core; generic PSKT combination must not need access to wallet storage. Add a wire extension only if the existing fields cannot express a required contract.
- Give KCC-3 unsigned bundle creation a path that does not load private keys. The current `Account::pskb_from_send_generator` loads a single `prv_key_data` and constructs a `PSKBSigner` even before signing, so watch-only/multisig cannot inherit it unchanged. Reuse the existing unsigned generator and transaction-to-PSKT conversion, then apply the explicit signing operation separately.
- Supply full per-key six-level origins in PSKT inputs and wallet-owned change outputs. For multisig, keep a separate local signature per cosigner at an address; the current address-to-one-key cache cannot represent this. Keep signing secrets scoped to the operation and use existing zeroization conventions.
- KCC-3 finalization orders valid signatures using the derived script-key order, verifies scheme and threshold, serializes the selected template's unlocking script, and appends the exact redeem script. Do not rely on compressed-public-key `BTreeMap` order for Schnorr x-only sorting, and do not finalize below threshold. Verify the resulting transaction with the existing txscript engine.
- Support unsigned construction with zero local keys, partial signing with some, and finalization/submission when the threshold is met. Direct send/sweep may only submit a fully signed result; otherwise return an explicit incomplete-signature result and use the bundle workflow.
- Use existing `Vec<PrvKeyDataArgs>` for per-local-key credentials in the KCC-3 bundle-signing request. The existing generic send interface's single payment secret must not be assumed sufficient for several independently protected seeds. Preserve that legacy interface; independently protected multisig keys can complete signing through explicit bundle operations. Any direct-send convenience must delegate to the same signing/finalization logic.
- Make mass/fee estimates include the selected redeem script and threshold signature pushes, and populate the correct sigop/compute commitments for supported transaction versions. `GeneratorSettings::try_new_with_account` currently carries only sigop count/minimum signatures; trace and extend the actual mass calculation where these do not account for the script bytes. Keep unrelated transaction generation and commit/reveal behavior unchanged or fail explicitly where the new account/policy is unsupported.

### 8. Creation, import/export, defaults, and recovery

| Existing interface / caller | Proposed change |
| --- | --- |
| `AccountCreateArgs`, `impl Wallet::create_account` | Append `Kcc3`, `Kcc3Watch`, and `Kcc3Multisig` variants with typed arguments. Do not reorder existing Borsh enum variants or alter their payloads. Network is explicit in KCC-3 public descriptions; creation against an active wallet checks it matches the selected network. |
| `PrvKeyData` and mnemonic import | Reuse an existing compatible key-data record when deriving another convention from the same seed material. Current `import_with_mnemonic` rejects an existing key-data ID; do not carry that rejection into KCC-3/legacy coexistence. Verify credentials/root derivation before reuse, and reject duplicate accounts instead of rejecting the reusable seed. |
| `ensure_default_account_impl` | Accept explicit old kinds and new `kcc3`; make callers that omit a kind choose KCC-3. Keep existing stored accounts when present. Do not redefine `new_bip32` or old key-generator constructors to mean KCC-3. |
| CLI wallet/account creation and mnemonic import | Update both `wizards/wallet.rs` (currently directly constructs BIP32) and account/import commands. Default to `kcc3`; retain an explicit account-kind selector in wallet-import and account-import flows. Keep legacy data-file import bound to its known convention. |
| WASM create/import conversion | Extend the declared TypeScript unions and actual conversion matches together; current create conversion only accepts BIP32/keypair. Expose all three new kinds and explicit scheme/policy/origin data; a missing kind in an applicable defaulting flow means KCC-3. |
| Account descriptors / events | Reuse existing descriptor properties, `Other(...)`, and JSON values for versioned KCC-3 context/policy/availability details where sufficient. Existing `AccountIndex`, `XpubKeys`, `Ecdsa`, and `DerivationMeta` describe the active context. Refresh descriptors on network changes without changing account IDs. |
| `AccountsDiscoveryKind` / `accounts_discovery_call` | Append KCC-3 Schnorr and ECDSA discovery choices. Preserve `Bip44` and its result contract. Each invocation scans one network/scheme subtree with the current UTXO loop and the same extent semantics; a higher-level recovery request invokes the selected schemes independently rather than collapsing their results into one global last-account index. |
| Existing account scan / import scan | Reuse both-chain scanning, windows, extents, and UTXO calls with the KCC-3 managers. Multisig scans an existing supplied policy; a watch xpub scans its supplied account. Neither enables hardened seed-only discovery of unknown cosigner policies/accounts. |
| Full-wallet backup / public account exchange | Extend payload/metadata serialization in the existing backup flow. Add a versioned public account-description DTO for single-context xpub or multisig-policy exchange; parse into the validated domain types before storing. |

Public description proposal: version `1`, convention `kcc3`, kind, and the selected network context's origin/public policy. Use canonical extended-key serialization and explicit scheme names; include multisig template and threshold. Keep wallet-local `PrvKeyDataId`, encryption/storage keys, secrets, and mutable activation state out of public exports. Full-wallet backups preserve the stable identity anchor and all saved contexts; importing a standalone public description may create a new watch-only identity rather than impersonating an existing seed-backed account.

Do not extend existing Borsh request structs by blindly appending fields: older byte sequences do not acquire defaults from Serde attributes. Prefer appended enum variants with their own new payloads, or explicitly versioned/additive operation messages for context provisioning and bundle signing. Check Rust, Borsh transport, WASM/TypeScript, and CLI compatibility separately.

Propose Schnorr as the default signature scheme, matching current wallet creation; keep ECDSA selectable. This is separate from the user-confirmed default account kind.

### 9. Function ownership audit for implementation

No new Rust functions are added in this planning pass. Proposed new operations below currently have zero call sites; the named consumers are planned implementation callers. Final implementation must record actual call counts and audit every added `fn`.

| Proposed operation | Owner | Planned consumers / invariant | Why it belongs outside a one-use orchestration block |
| --- | --- | --- | --- |
| `AccountDerivation::try_new`, path conversion, `account_path`, `address_path`, account-key derivation | Inherent impl in wallet-keys | Creation, import, private signing, recovery; registry/path/crypto boundary | Central KCC-3 invariant used across public and private derivation. |
| `AccountPublicKey::try_new` | Inherent impl in wallet-keys | Watch import, cosigner import, storage reload; origin/xpub boundary | All untrusted public account keys need the same structural checks. |
| `PrvKeyData::create_kcc3_xpub` | Existing inherent impl | Account/context creation and discovery; encrypted seed-to-account-key boundary | Key-data owner already controls decryption and master-key access. |
| Policy constructor, `MultisigPolicy::derive_script`, canonical identity encoding | Inherent impls on policy / KCC-3 payload | Address generation, signers, export/import, account hashes | Own threshold, ordering, script bytes, and identity serialization; multiple real consumers. |
| Per-network metadata selection/update methods | Existing `impl AccountMetadata` | Reload, new address, scan updates, network switch | Owns preservation of the complete progress map. |
| `Account::ensure_active_context`, signer construction hook | Existing trait with KCC-3 overrides | Address/scan/sign/generation/submission and send/sweep | Encodes runtime validity and signer capability where callers already use dynamic accounts. |
| Scheme-aware signing and input finalization | `impl Kcc3Signer` | Direct transaction signer and PSKB adapter | Meaningful cryptographic/serialization boundaries with shared input-policy validation. |
| Account-aware network transition | Existing `impl Wallet` | API network change, connect, CLI, WASM | Owns wallet lifecycle, guards, generations, and active account map. |

Keep the bodies of creation/import/API/CLI orchestration in their existing callers. No new module-level orchestration helpers, argument-bundle structs, generic service layer, or new dependency is proposed. Small loops selecting schemes, deriving cosigners, or choosing contexts stay with the owning method unless actual duplication or a domain boundary justifies extraction.

## Ordered implementation checklist

Each step is a reviewable change; this document does not start these changes.

| Step | Scope and main files | Acceptance evidence |
| --- | --- | --- |
| 1. Derivation contract | New wallet-keys KCC-3 module; existing BIP32 primitives only where required | Authoritative derivation/watch vectors for four networks and both schemes; exact path validation, bounds, private/public equality; resolve and fault-test invalid-child behavior before claiming conformance. Gen0/gen1 vectors unchanged. |
| 2. Account policy and identities | New KCC-3 variant/policy modules, kinds/factories, new payload hash encoding | Both homogeneous script templates; per-address encoded sorting and permutations; thresholds/duplicates/mixed schemes rejected; anchor identity stable under annotation/context/local-key changes. |
| 3. Persistence and read-only lifecycle | New payload, versioned `AccountMetadata`, factory load, descriptors | Version-zero wallets/metadata still read and legacy writes remain byte-stable; all contexts and counters survive native/WASM backup/reload; unavailable contexts enumerate without an address. |
| 4. Address managers and network lifecycle | `derivation.rs`, account trait callers, wallet API/network routes, CLI/WASM setter | P2PK/watch equality; multisig nonzero ranges and scanner second window; mainnet -> testnet -> mainnet restores counters; testnet-instance change preserves keys but clears chain state; missing/locked contexts, stale handles, and failed transitions behave explicitly. |
| 5. P2PK create/import and spending | Wallet creation/import dispatch, signer hook, `Kcc3Signer`, transaction generation | Schnorr and ECDSA funding -> signing -> script verification; watch unsigned construction and signing rejection; same seed supports old and new conventions; context guards prevent cross-network use. |
| 6. Multisig spending and bundles | KCC-3 signer/policy, PSKB adapter/finalization, mass estimation | Two wallets independently derive identical addresses and combine threshold signatures; zero/some/enough local keys; distinct credential handling; signatures follow script order across address changes; below-threshold finalization rejected; real script verification and estimated/final mass comparison for both schemes. |
| 7. Discovery and interface completion | Existing discovery loop/caller, all API/WASM/CLI create/import/export paths | Existing UTXO scan calls, windows, extents, and stopping behavior retained; empty Schnorr subtree does not suppress ECDSA scan; explicit legacy recovery unchanged; public descriptions round-trip without secrets/local storage identifiers. |
| 8. Defaults and release checks | Default-account initialization, CLI wallet/account wizards, WASM defaults, docs | New unqualified creation/import uses `kcc3`; every legacy choice still selects its old path; existing wallet/typed backup kinds remain unchanged. Enable these defaults only once the preceding flows work. |

Run focused tests as each step lands, then the ready patch's broader checks. Use the targeted crate commands below together with the repository's workspace/WASM CI checks:

```text
cargo test -p kaspa-wallet-keys -p kaspa-wallet-core -p kaspa-wallet-pskt
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --features devnet-prealloc --workspace --all --tests --benches
cargo nextest run --workspace
cargo test --doc --workspace
cargo clippy -p kaspa-wallet-cli-wasm --target wasm32-unknown-unknown
cargo clippy -p kaspa-wasm --target wasm32-unknown-unknown
wasm-pack test --node wallet/pskt
wasm-pack test --node wallet/core --features wasm32-sdk
```

Use existing RPC mocks to verify scan requests and independent subtree stopping. Use real existing txscript-engine verification for spend correctness rather than a mock signature verifier. Relevant script-helper changes additionally need their txscript tests; record actual command results and environment blockers during implementation. No Rust build/test suite is claimed by this planning document.

## Review points before freezing the Rust design

1. Review the proposed stable identity anchor plus per-network contexts, including independently allocated active-network account indices and runtime replacement on switching.
2. Review the necessary versioned metadata extension, fallible derivation accessor, and async live-wallet network setter. These are the principal compatibility changes; exact DTOs and field encodings should be frozen before the storage/API steps.
3. Current-phase invalid-child behavior is settled: reject every invalid child, matching existing derivation. Clarifying the draft specification remains a separate interoperability question; legacy derivation is unchanged.
4. Review the proposed Schnorr default, homogeneous script-template identifiers, version-one public description, and multisig credential/bundle workflow. They refine the agreed scope rather than adding mixed-scheme support or historical recovery.

Reviewed local inputs for this architecture pass (SHA-256):

- Specification: `64c9d43e83cbaa72b272c6ea99a0d736be34498982024e3ff07fd5244c2e4e13`.
- Conformance vectors: `c938b3449f8c6782c194785b7300c5ddb7862b8bfd94e57d8605c80e647be793`.

No consensus change, automatic migration of funds, new recovery backend, mixed-scheme multisig, or unrelated wallet refactor is part of this plan.


## Invalid-child policy follow-up

The retry helpers were removed from wallet-keys. Account creation and network provisioning use `PrvKeyData::create_kcc3_xpub` for the exact allocated origin; discovery propagates its derivation error. Address ranges use the existing fallible public-key-manager collection, and private-key ranges finish derivation before caching addresses. No new production functions or dependencies were introduced. The added test-only manager implements the existing trait; the renamed key test retains real invalid-tweak rejection checks at all six depths. Follow-up validation passed: `cargo test -p kaspa-wallet-keys -p kaspa-wallet-core -p kaspa-wallet-pskt` (79 native tests and doc-test targets), `wasm-pack test --node wallet/core --features wasm32-sdk` (13 WASM tests), `cargo clippy -p kaspa-wallet-keys -p kaspa-wallet-core -p kaspa-cli --all-targets --features kaspa-wallet-core/wasm32-sdk -- -D warnings`, `cargo fmt --check`, and `git diff --check`.


## Account ownership follow-up

Removed the four KCC-3-specific methods from the native `Wallet` implementation. `Kcc3` now owns index allocation, context provisioning and discovery. `AccountImportArgs` owns construction dispatch for both mnemonic imports and initial account creation; `Wallet::import_mnemonic` retains shared seed verification/reuse, locking, storage and startup. CLI/WASM callers use these owners, and the kind-based `Wallet::import_with_mnemonic` signature remains available. The existing WASM `provisionKcc3Context` binding is unchanged.

Validation passed after the refactor: `cargo test -p kaspa-wallet-core -p kaspa-wallet-keys -p kaspa-wallet-pskt` (79 native tests plus doc-test targets), `wasm-pack test --node wallet/core --features wasm32-sdk` (13 WASM tests), `cargo clippy -p kaspa-wallet-keys -p kaspa-wallet-core -p kaspa-cli --all-targets --features kaspa-wallet-core/wasm32-sdk -- -D warnings`, `cargo fmt --check`, and `git diff --check`. The full workspace matrix remains unrun; validation covers the affected wallet crates, CLI and WASM runtime.


## Wallet implementation review follow-up

Reviewed the complete `wallet/core/src/wallet/mod.rs` diff and its account/API callers. Corrected the accidental legacy support in `ensure_default_account_impl` and initial wallet creation; the latter rejects unsupported kinds before closing the current wallet. Existing explicit legacy imports remain supported.

The review also corrected transition integration: reuse initialized legacy account caches, enumerate missing-network accounts without attempting startup, retain explicit activation requests until a supported network is selected, and remove both active and unavailable account IDs on deactivation. Deactivation uses its caller-provided wallet guard instead of re-locking it. Reset now takes the existing context lock before cleanup so it serializes with in-flight operations. The new context lock/generation accessors are crate-visible. The synchronous network setter also rejects changes while remembered activations exist, even if every old runtime handle was dropped, so it cannot bypass restoration on the destination network.

No new production functions or argument types were added in this follow-up. Four test-only integration functions own the regression cases: `default_account_creation_rejects_unsupported_kinds`, `network_transitions_preserve_legacy_public_cache`, `reset_waits_for_in_flight_context_and_invalidates_views`, and `unavailable_accounts_remain_enumerable_and_can_be_deactivated`. They exercise the public wallet API and lifecycle behavior, including empty/existing storage and explicit/all-account deactivation. The first two tests reproduced the original failures before the corresponding fixes. The user is independently extracting KCC-3 derivation; this review leaves that module to the user.


Final validation for this review passed: `cargo test -p kaspa-wallet-core -p kaspa-wallet-keys -p kaspa-wallet-pskt` (84 native tests and doc-test targets), `wasm-pack test --node wallet/core --features wasm32-sdk` (18 WASM tests), `cargo clippy -p kaspa-wallet-keys -p kaspa-wallet-core -p kaspa-cli --all-targets --features kaspa-wallet-core/wasm32-sdk -- -D warnings`, `cargo fmt --check`, and `git diff --check`. The native/WASM runs include the user's concurrent derivation extraction and the final synchronous-setter regression. Checks remain scoped to the affected wallet crates and CLI rather than the full workspace matrix.
