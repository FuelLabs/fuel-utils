use fuels::{
    accounts::{
        Account,
        ViewOnlyAccount,
        provider::Provider,
        signers::private_key::PrivateKeySigner,
        wallet::Wallet,
    },
    crypto::{
        Message,
        SecretKey,
        Signature,
    },
    types::{
        AssetId,
        bech32::Bech32Address,
        coin_type_id::CoinTypeId,
        errors::Error,
        input::Input,
        transaction_builders::TransactionBuilder,
    },
};
use fuels_core::traits::Signer;

use crate::kms_wallet::KMSWallet;

#[derive(Clone, Debug)]
pub enum UpgraderWallet {
    Kms(KMSWallet),
    WalletUnlocked(Wallet),
}

impl UpgraderWallet {
    pub async fn from_kms_key_id(
        kms_key_id: String,
        provider: Provider,
    ) -> anyhow::Result<Self> {
        Ok(UpgraderWallet::Kms(
            KMSWallet::from_kms_key_id(kms_key_id, provider).await?,
        ))
    }

    pub fn from_secret_key(private_key: SecretKey, provider: Provider) -> Self {
        UpgraderWallet::WalletUnlocked(Wallet::new(
            PrivateKeySigner::new(private_key),
            provider,
        ))
    }

    pub fn provider(&self) -> &Provider {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.provider(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.provider(),
        }
    }

    pub fn address(&self) -> &Bech32Address {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.address(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.address(),
        }
    }
}

#[async_trait::async_trait]
impl ViewOnlyAccount for UpgraderWallet {
    async fn get_asset_inputs_for_amount(
        &self,
        asset_id: AssetId,
        amount: u128,
        excluded_coins: Option<Vec<CoinTypeId>>,
    ) -> Result<Vec<Input>, Error> {
        match self {
            UpgraderWallet::Kms(wallet) => {
                wallet
                    .get_asset_inputs_for_amount(asset_id, amount, excluded_coins)
                    .await
            }
            UpgraderWallet::WalletUnlocked(wallet) => {
                wallet
                    .get_asset_inputs_for_amount(asset_id, amount, excluded_coins)
                    .await
            }
        }
    }

    fn address(&self) -> &Bech32Address {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.address(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.address(),
        }
    }

    fn try_provider(&self) -> Result<&Provider, Error> {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.try_provider(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.try_provider(),
        }
    }
}

#[async_trait::async_trait]
impl Account for UpgraderWallet {
    fn add_witnesses<Tb: TransactionBuilder>(&self, tb: &mut Tb) -> Result<(), Error> {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.add_witnesses(tb),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.add_witnesses(tb),
        }
    }
}

#[async_trait::async_trait]
impl Signer for UpgraderWallet {
    async fn sign(&self, message: Message) -> Result<Signature, Error> {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.sign(message).await,
            UpgraderWallet::WalletUnlocked(wallet) => wallet.signer().sign(message).await,
        }
    }

    fn address(&self) -> &Bech32Address {
        match self {
            UpgraderWallet::Kms(wallet) => wallet.address(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.address(),
        }
    }
}
