use kaspa_addresses::Prefix;
use kaspa_bip32::{secp256k1::PublicKey, AddressType, ChildNumber, ExtendedPrivateKey, Prefix as KeyPrefix, PrivateKey, SecretKey};
use kaspa_consensus_core::constants::TX_VERSION;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::sign::{sign_with_multiple_v2, Signed};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_rpc_core::{api::rpc::RpcApi, RpcAddress};
use kaspa_txscript::{extract_script_pub_key_address, pay_to_address_script, pay_to_script_hash_script};
use kaspa_wallet_core::{
    derivation::WalletDerivationManagerTrait,
    prelude::{Language, Mnemonic},
    utils::try_kaspa_str_to_sompi,
};
use kaspa_wallet_keys::derivation::gen1::{PubkeyDerivationManager, WalletDerivationManager};
use kaspa_wrpc_client::{
    client::{ConnectOptions, ConnectStrategy},
    prelude::NetworkId,
    prelude::NetworkType,
    result::Result,
    KaspaRpcClient, WrpcEncoding,
};
use std::process::ExitCode;
use std::time::Duration;

use crate::covenant::{build_covenant_script, build_spend_tx, encode_counter, CovenantState};

mod covenant;

const ACCOUNT_INDEX: u64 = 1;

struct WalletContext {
    receive_prv_generator: ExtendedPrivateKey<SecretKey>,
    change_prv_generator: ExtendedPrivateKey<SecretKey>,
    hd_wallet: WalletDerivationManager,
}

impl WalletContext {
    fn from_mnemonic_str(mnemonic_str: &str) -> Self {
        let mnemonic = Mnemonic::new(mnemonic_str, Language::English).expect("Error: provided <mnemonic> isn't a valid mnemonic.");
        let master_xprv =
            ExtendedPrivateKey::<SecretKey>::new(mnemonic.to_seed("")).expect("Error while building xprv from <mnemonic>.");

        let hd_wallet =
            WalletDerivationManager::from_master_xprv(master_xprv.to_string(KeyPrefix::XPRV).as_str(), false, ACCOUNT_INDEX, None)
                .expect("Error while getting hd wallet from xprv.");

        let receive_prv_generator: ExtendedPrivateKey<SecretKey> = master_xprv
            .clone()
            .derive_path(
                &WalletDerivationManager::build_derivate_path(false, ACCOUNT_INDEX, None, Some(kaspa_bip32::AddressType::Receive))
                    .expect("Error while building derivate path."),
            )
            .expect("Error while derive path from xprv.");

        let change_prv_generator: ExtendedPrivateKey<SecretKey> = master_xprv
            .clone()
            .derive_path(
                &WalletDerivationManager::build_derivate_path(false, ACCOUNT_INDEX, None, Some(kaspa_bip32::AddressType::Change))
                    .expect("Error while building derivate path."),
            )
            .expect("Error while derive path from xprv.");

        WalletContext { receive_prv_generator, change_prv_generator, hd_wallet }
    }

    fn get_pubkey_at_index(&self, index: u32, address_type: AddressType) -> PublicKey {
        let derivation_manager = match address_type {
            AddressType::Receive => self.hd_wallet.receive_pubkey_manager(),
            AddressType::Change => self.hd_wallet.change_pubkey_manager(),
        };

        derivation_manager.derive_pubkey(index).expect("Error while derive first pubkey.")
    }

    fn get_prvkey_at_index(&self, index: u32, address_type: AddressType) -> impl PrivateKey {
        let xprv_generator = match address_type {
            AddressType::Receive => &self.receive_prv_generator,
            AddressType::Change => &self.change_prv_generator,
        };

        xprv_generator
            .derive_child(ChildNumber::new(index, false).expect("Error while getting child number"))
            .expect("Error while derive first pubkey.")
            .private_key()
            .to_owned()
    }
}

#[tokio::main]
/// cargo run -p kaspa-wrpc-covenants-counter -- <mnemonic>
async fn main() -> ExitCode {
    let mnemonic_arg = std::env::args().nth(1).expect("Error: a <mnemonic> argument was not provided.");

    let wallet_ctx = WalletContext::from_mnemonic_str(&mnemonic_arg);

    match create_covenants(&wallet_ctx).await {
        Ok(_) => {
            println!("Well done! You successfully created and sent one covenant utxo!");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("An error occurred: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn create_covenants(wallet_ctx: &WalletContext) -> Result<()> {
    let encoding = WrpcEncoding::Borsh;

    let url = Some("ws://127.0.0.1:17610");
    let resolver = None;

    let network_type = NetworkType::Devnet;
    let selected_network = Some(NetworkId::new(network_type));

    // Advanced options
    let subscription_context = None;

    // Create new wRPC client with parameters defined above
    let client = KaspaRpcClient::new(encoding, url, resolver, selected_network, subscription_context)?;

    // Advanced connection options
    let timeout = 5_000;
    let options = ConnectOptions {
        block_async_connect: true,
        connect_timeout: Some(Duration::from_millis(timeout)),
        strategy: ConnectStrategy::Fallback,
        ..Default::default()
    };

    // get covenant details
    let covenant_redeem_script = build_covenant_script().expect("Cannot build the covenant script.");
    let covenant_p2sh_script = pay_to_script_hash_script(covenant_redeem_script.as_slice());
    let covenant_p2sh_address =
        extract_script_pub_key_address(&covenant_p2sh_script, Prefix::Devnet).expect("Cannot get address from p2sh script");

    println!("covenant address: {}", covenant_p2sh_address);

    // get wallet receive address 0
    let first_schnorr_address =
        PubkeyDerivationManager::create_address(&wallet_ctx.get_pubkey_at_index(0, AddressType::Receive), Prefix::Devnet, false)
            .expect("Cannot get address from pubkey");

    println!("wallet first address: {}", first_schnorr_address);

    client.connect(Some(options)).await?;

    // get utxos
    let result = client
        .get_utxos_by_addresses(vec![RpcAddress::from(first_schnorr_address.clone())])
        .await
        .expect("Cannot find utxo by address");

    // create transaction and send it to covenant address
    let amount = try_kaspa_str_to_sompi("1".to_string()).expect("Cannot convert kaspa to sompi.").unwrap();

    let utxo = result.iter().find(|utxo| utxo.utxo_entry.amount >= amount).expect("No satisfied UTXO found.");

    println!("selected utxo: {:?}", utxo);

    let utxo_entry: UtxoEntry = utxo.utxo_entry.clone().into();
    let outpoint: TransactionOutpoint = utxo.outpoint.into();

    let change_value = utxo_entry.amount.saturating_sub(amount);
    let change_script = pay_to_address_script(&first_schnorr_address);

    // temporarily create output without fees removed to calculate fees
    let mut temp_outputs = vec![TransactionOutput::new(amount, covenant_p2sh_script.clone())];
    if change_value > 0 {
        temp_outputs.push(TransactionOutput::new(change_value, change_script.clone()));
    }

    let input = TransactionInput::new(outpoint, vec![], 0, 1);
    let temp_tx = Transaction::new(TX_VERSION, vec![input.clone()], temp_outputs, 0, SUBNETWORK_ID_NATIVE, 0, encode_counter(0));
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![utxo_entry.clone()]);

    // calculate fees
    let calculator = MassCalculator::new_with_consensus_params(&NetworkType::Devnet.into());
    let storage_mass = calculator.calc_contextual_masses(&temp_tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(temp_tx.tx);
    let mass = storage_mass.max(compute_mass).max(transient_mass);

    let change_value = utxo_entry.amount.saturating_sub(amount).saturating_sub(mass);

    // now create the real transaction with calculated fees
    let mut outputs = vec![TransactionOutput::new(amount, covenant_p2sh_script.clone())];
    if change_value > 0 {
        outputs.push(TransactionOutput::new(change_value, change_script));
    }

    let input = TransactionInput::new(outpoint, vec![], 0, 1);
    let funding_tx = Transaction::new(TX_VERSION, vec![input.clone()], outputs, 0, SUBNETWORK_ID_NATIVE, 0, encode_counter(0));

    let prvkey = wallet_ctx.get_prvkey_at_index(0, AddressType::Receive).to_bytes();
    let signable = SignableTransaction::with_entries(funding_tx, vec![utxo_entry]);
    let signed = match sign_with_multiple_v2(signable, &[prvkey]) {
        Signed::Fully(tx) => tx,
        Signed::Partially(_) => panic!("Funding tx signing was partial (missing keys for inputs)"),
    };
    let funding_tx = signed.tx;
    let funding_tx_id = client.submit_transaction((&funding_tx).into(), false).await?;
    println!("funding tx submitted: {funding_tx_id}");

    let covenant_entry = UtxoEntry::new(funding_tx.outputs[0].value, covenant_p2sh_script.clone(), 0, false);
    let state = CovenantState::from_tx_with_entry(funding_tx.clone(), covenant_entry, 0);
    let spend_tx = build_spend_tx(&state, 1, &covenant_p2sh_script, covenant_redeem_script.as_slice());

    let spend_tx_id = client.submit_transaction((&spend_tx).into(), true).await?;
    println!("spend tx submitted: {spend_tx_id}, current state: {}", state.counter);

    // Disconnect client from Kaspa node
    client.disconnect().await?;

    // Return function result
    Ok(())
}
