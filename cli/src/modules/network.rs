use crate::imports::*;
use kaspa_wallet_core::account::{KCC3_ACCOUNT_KIND, KCC3_WATCH_ACCOUNT_KIND, kcc3::Kcc3};

#[derive(Default, Handler)]
#[help("Select network type (mainnet|testnet)")]
pub struct Network;

impl Network {
    async fn main(self: Arc<Self>, ctx: &Arc<dyn Context>, argv: Vec<String>, _cmd: &str) -> Result<()> {
        let ctx = ctx.clone().downcast_arc::<KaspaCli>()?;

        if let Some(network_id) = argv.first() {
            let network_id: NetworkId = network_id.trim().parse::<NetworkId>()?;
            let wallet = ctx.wallet();
            let selected = wallet.account().ok().map(|account| *account.id());
            tprintln!(ctx, "Setting network id to: {network_id}");
            wallet.change_network_context(&network_id).await?;
            wallet.settings().set(WalletSettings::Network, network_id).await?;

            if wallet.is_open() {
                let descriptors = {
                    let guard = wallet.guard();
                    let guard = guard.lock().await;
                    wallet.clone().account_descriptors(&guard).await?
                };
                for descriptor in descriptors {
                    if !matches!(descriptor.kind.as_ref(), KCC3_ACCOUNT_KIND | KCC3_WATCH_ACCOUNT_KIND)
                        || descriptor.receive_address.is_some()
                    {
                        continue;
                    }
                    tprintln!(ctx, "Set up account {} on {network_id}", descriptor.name_with_id());
                    let watch = descriptor.kind == KCC3_WATCH_ACCOUNT_KIND;
                    let description = if watch {
                        let Some(description) = crate::wizards::import::prompt_for_kcc3_public_account(
                            &ctx.term(),
                            network_id.network_type,
                            "Destination extended public key (leave empty to skip): ",
                        )
                        .await?
                        else {
                            continue;
                        };
                        Some(description.try_key()?)
                    } else {
                        None
                    };
                    let account = {
                        let guard = wallet.guard();
                        let guard = guard.lock().await;
                        wallet
                            .get_account_by_id(&descriptor.account_id, &guard)
                            .await?
                            .ok_or_else(|| Error::custom("Account not found"))?
                    };
                    let (wallet_secret, payment_secret) = ctx.ask_wallet_secret(if watch { None } else { Some(&account) }).await?;
                    Kcc3::provision_context(&wallet, &wallet_secret, account.id(), payment_secret.as_ref(), description).await?;
                    let guard = wallet.guard();
                    let guard = guard.lock().await;
                    wallet.activate_accounts(Some(&[descriptor.account_id]), &guard).await?;
                    if selected == Some(descriptor.account_id) {
                        let account = wallet
                            .get_account_by_id(&descriptor.account_id, &guard)
                            .await?
                            .ok_or_else(|| Error::custom("Account not found"))?;
                        wallet.select(Some(&account)).await?;
                    }
                }
            }
        } else {
            let network_id = ctx.wallet().network_id()?;
            tprintln!(ctx, "Current network id is: {network_id}");
        }

        Ok(())
    }
}
