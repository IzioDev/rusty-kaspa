use crate::imports::*;
use kaspa_wallet_core::account::{MULTISIG_ACCOUNT_KIND, multisig::MultiSig};
use kaspa_wallet_core::derivation::MultisigDerivationScheme;
use kaspa_wallet_core::storage::keydata::PrvKeyDataVariantKind;
use kaspa_wallet_pskt::prelude::Bundle;

#[derive(Default, Handler)]
#[help("Multisig account and PSKB ceremony operations")]
pub struct Multisig;

impl Multisig {
    async fn main(self: Arc<Self>, ctx: &Arc<dyn Context>, mut argv: Vec<String>, _cmd: &str) -> Result<()> {
        let ctx = ctx.clone().downcast_arc::<KaspaCli>()?;

        if !ctx.wallet().is_open() {
            return Err(Error::WalletIsNotOpen);
        }

        if argv.is_empty() {
            return self.display_help(ctx, argv).await;
        }

        let action = argv.remove(0);
        match action.as_str() {
            "init" => self.init(ctx, argv).await,
            "signer-xpub" => self.signer_xpub(ctx, argv).await,
            "finalize" => self.finalize(ctx, argv).await,
            "xpub" => self.xpub(ctx).await,
            "pskb" => self.pskb(ctx, argv).await,
            v => {
                tprintln!(ctx, "unknown command: '{v}'\r\n");
                self.display_help(ctx, argv).await
            }
        }
    }

    async fn init(self: Arc<Self>, ctx: Arc<KaspaCli>, argv: Vec<String>) -> Result<()> {
        if argv.len() > 1 {
            return self.display_help(ctx, argv).await;
        }

        let signer_name = argv.first().map(|value| value.trim()).filter(|value| !value.is_empty()).map(String::from);
        let (wallet_secret, _) = ctx.ask_wallet_secret(None).await?;
        let payment_secret = Self::ask_optional_payment_secret(&ctx).await?;

        let mnemonic = Mnemonic::random(WordCount::Words24, Language::default())?;
        let mnemonic_phrase = mnemonic.phrase().to_string();
        let prv_key_data_args = PrvKeyDataCreateArgs::new(
            signer_name,
            payment_secret.clone(),
            Secret::from(mnemonic_phrase.as_str()),
            PrvKeyDataVariantKind::Mnemonic,
        );

        let wallet = ctx.wallet();
        let prv_key_data_id = wallet.create_prv_key_data(&wallet_secret, prv_key_data_args).await?;
        let xpub = Self::load_multisig_xpub(&ctx, &wallet_secret, &prv_key_data_id, payment_secret.as_ref()).await?;

        tprintln!(ctx, "");
        tprintln!(ctx, "multisig signer created: {}", prv_key_data_id.to_hex());
        tprintln!(ctx, "");
        tprintln!(ctx, "IMPORTANT: back up this signer mnemonic now.");
        tprintln!(ctx, "");
        tprintln!(ctx, "mnemonic:");
        tprintln!(ctx, "");
        ctx.term().writeln(mnemonic_phrase);
        tprintln!(ctx, "");
        tprintln!(ctx, "multisig xpub:");
        tprintln!(ctx, "");
        tprintln!(ctx, "{xpub}");
        tprintln!(ctx, "");

        Ok(())
    }

    async fn signer_xpub(self: Arc<Self>, ctx: Arc<KaspaCli>, argv: Vec<String>) -> Result<()> {
        if !argv.is_empty() {
            return self.display_help(ctx, argv).await;
        }

        let prv_key_data_info = ctx.select_private_key().await?;
        let (wallet_secret, payment_secret) = Self::ask_wallet_secret_for_key(&ctx, &prv_key_data_info).await?;
        let xpub = Self::load_multisig_xpub(&ctx, &wallet_secret, &prv_key_data_info.id, payment_secret.as_ref()).await?;

        tprintln!(ctx, "{xpub}");

        Ok(())
    }

    async fn finalize(self: Arc<Self>, ctx: Arc<KaspaCli>, argv: Vec<String>) -> Result<()> {
        if argv.len() > 1 {
            return self.display_help(ctx, argv).await;
        }

        let account_name = argv.first().map(|value| value.trim()).filter(|value| !value.is_empty()).map(String::from);
        let prv_key_data_info = ctx.select_private_key().await?;
        let (wallet_secret, payment_secret) = Self::ask_wallet_secret_for_key(&ctx, &prv_key_data_info).await?;

        let minimum_signatures: u16 =
            ctx.term().ask(false, "Enter the minimum number of signatures required: ").await?.trim().parse()?;

        let mut additional_xpub_keys = Vec::new();

        loop {
            let xpub = ctx.term().ask(false, "Enter peer extended public key (empty to finish): ").await?;
            let xpub = xpub.trim();
            if xpub.is_empty() {
                break;
            }

            additional_xpub_keys.push(xpub.to_string());
        }

        if additional_xpub_keys.is_empty() {
            return Err(Error::custom("At least one peer xpub is required"));
        }

        let signer_count = additional_xpub_keys.len() + 1;
        if minimum_signatures == 0 {
            return Err(Error::custom("Minimum signatures must be greater than zero"));
        }
        if minimum_signatures as usize > signer_count {
            return Err(Error::custom(format!(
                "Minimum signatures ({minimum_signatures}) cannot exceed signer count ({signer_count})"
            )));
        }

        let prv_key_data_args = vec![PrvKeyDataArgs::new(prv_key_data_info.id, payment_secret)];
        let account = ctx
            .wallet()
            .create_account_multisig(
                &wallet_secret,
                prv_key_data_args,
                additional_xpub_keys,
                account_name,
                minimum_signatures,
                MultisigDerivationScheme::default(),
            )
            .await?;

        tprintln!(ctx, "\naccount created: {}\n", account.get_list_string()?);
        ctx.wallet().select(Some(&account)).await?;
        tprintln!(ctx, "deposit address:");
        tprintln!(ctx, "{}", style(account.receive_address()?).blue());
        tprintln!(ctx, "");

        Ok(())
    }

    async fn xpub(self: Arc<Self>, ctx: Arc<KaspaCli>) -> Result<()> {
        let account = ctx.wallet().account()?;
        let Some(xpub_keys) = account.xpub_keys() else {
            return Err(Error::custom("Selected account does not expose multisig xpub keys"));
        };

        for (index, xpub) in xpub_keys.iter().enumerate() {
            tprintln!(ctx, "{}: {}", index, xpub);
        }
        Ok(())
    }

    async fn pskb(self: Arc<Self>, ctx: Arc<KaspaCli>, mut argv: Vec<String>) -> Result<()> {
        if argv.is_empty() {
            return self.display_help(ctx, argv).await;
        }

        let account = ctx.wallet().account()?;
        let dyn_account = account.clone();
        let account = account.downcast_arc::<MultiSig>()?;
        let action = argv.remove(0);

        match action.as_str() {
            "create" => {
                if argv.len() < 2 || argv.len() > 3 {
                    return self.display_help(ctx, argv).await;
                }

                let address = Address::try_from(argv.first().unwrap().as_str())?;
                let amount_sompi = try_parse_required_nonzero_kaspa_as_sompi_u64(argv.get(1))?;
                let priority_fee_sompi = try_parse_optional_kaspa_as_sompi_i64(argv.get(2))?.unwrap_or(0);
                let abortable = Abortable::default();
                let outputs = PaymentOutputs::from((address, amount_sompi));

                let pskb = account.pskb_from_multisig(outputs.into(), None, priority_fee_sompi.into(), None, &abortable).await?;
                tprintln!(ctx, "{}", pskb.serialize()?);
                Ok(())
            }
            "sign" => {
                if argv.len() != 1 {
                    return self.display_help(ctx, argv).await;
                }

                let pskb = Self::parse_input_pskb(argv.first().unwrap())?;
                let (wallet_secret, payment_secret) = ctx.ask_wallet_secret(Some(&dyn_account)).await?;
                let signed = account.sign_multisig_pskb(&pskb, wallet_secret, payment_secret).await?;
                tprintln!(ctx, "{}", signed.serialize()?);
                Ok(())
            }
            "combine" => {
                if argv.len() < 2 {
                    return self.display_help(ctx, argv).await;
                }

                let pskb = Self::combine_inputs(argv)?;
                tprintln!(ctx, "{}", pskb.serialize()?);
                Ok(())
            }
            "broadcast" => {
                if argv.is_empty() {
                    return self.display_help(ctx, argv).await;
                }

                let pskb = Self::combine_inputs(argv)?;
                let transaction_ids = account.finalize_and_submit_multisig_pskb(&pskb).await?;
                tprintln!(ctx, "Sent transactions {:?}", transaction_ids);
                Ok(())
            }
            v => {
                tprintln!(ctx, "unknown multisig pskb command: '{v}'\r\n");
                self.display_help(ctx, argv).await
            }
        }
    }

    async fn ask_optional_payment_secret(ctx: &Arc<KaspaCli>) -> Result<Option<Secret>> {
        let payment_secret = ctx.term().ask(true, "Enter bip39 mnemonic passphrase (optional): ").await?;
        let payment_secret = payment_secret.trim();
        Ok((!payment_secret.is_empty()).then(|| Secret::new(payment_secret.as_bytes().to_vec())))
    }

    async fn ask_wallet_secret_for_key(ctx: &Arc<KaspaCli>, prv_key_data_info: &PrvKeyDataInfo) -> Result<(Secret, Option<Secret>)> {
        let (wallet_secret, _) = ctx.ask_wallet_secret(None).await?;
        let payment_secret = if prv_key_data_info.requires_bip39_passphrase() {
            let payment_secret = Secret::new(ctx.term().ask(true, "Enter payment password: ").await?.trim().as_bytes().to_vec());
            if payment_secret.as_ref().is_empty() {
                return Err(Error::PaymentSecretRequired);
            }
            Some(payment_secret)
        } else {
            None
        };

        Ok((wallet_secret, payment_secret))
    }

    async fn load_multisig_xpub(
        ctx: &Arc<KaspaCli>,
        wallet_secret: &Secret,
        prv_key_data_id: &PrvKeyDataId,
        payment_secret: Option<&Secret>,
    ) -> Result<String> {
        let prv_key_data =
            ctx.store().as_prv_key_data_store()?.load_key_data(wallet_secret, prv_key_data_id).await?.ok_or(Error::KeyDataNotFound)?;

        let xpub = prv_key_data
            .create_xpub(payment_secret, MULTISIG_ACCOUNT_KIND.into(), 0, Some(MultisigDerivationScheme::default()))
            .await?;
        Ok(ctx.wallet().network_format_xpub(&xpub))
    }

    fn parse_input_pskb(input: &str) -> Result<Bundle> {
        Bundle::try_from(input).map_err(|error| Error::custom(format!("Error while parsing input PSKB {error}")))
    }

    fn combine_inputs(inputs: Vec<String>) -> Result<Bundle> {
        let mut iter = inputs.iter();
        let first = iter.next().ok_or_else(|| Error::custom("Missing PSKB input"))?;
        let mut combined = Self::parse_input_pskb(first)?;

        for input in iter {
            let next = Self::parse_input_pskb(input)?;
            combined = combined.combine(next)?;
        }

        Ok(combined)
    }

    async fn display_help(self: Arc<Self>, ctx: Arc<KaspaCli>, _argv: Vec<String>) -> Result<()> {
        ctx.term().help(
            &[
                ("multisig init [signer name]", "Create a local multisig signer and print the xpub to share"),
                ("multisig signer-xpub", "Print a selected private key's multisig xpub"),
                ("multisig finalize [account name]", "Create a multisig account after collecting peer xpubs"),
                ("multisig xpub", "Print selected multisig account xpub keys"),
                ("multisig pskb create <address> <amount> [priority fee]", "Create an unsigned multisig PSKB"),
                ("multisig pskb sign <pskb>", "Add local multisig signatures and print the updated PSKB"),
                ("multisig pskb combine <pskb> <pskb...>", "Combine signed multisig PSKBs"),
                ("multisig pskb broadcast <pskb> [pskb...]", "Combine, finalize, validate, and broadcast multisig PSKBs"),
            ],
            None,
        )?;
        Ok(())
    }
}
