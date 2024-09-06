use fuels::{
    accounts::{
        provider::Provider,
        wallet::WalletUnlocked,
        Account,
        ViewOnlyAccount,
    },
    crypto::{SecretKey, Signature, Message},
    types::{bech32::Bech32Address, errors::Error, AssetId, input::Input, coin_type_id::CoinTypeId, transaction_builders::TransactionBuilder},
};
use fuels_core::traits::Signer;

use crate::kms_wallet::KMSWallet;

#[derive(Clone, Debug)]
pub enum UpgraderWallet {
    KMS(KMSWallet),
    WalletUnlocked(WalletUnlocked),
}

impl UpgraderWallet {
    pub async fn from_kms_key_id(
        kms_key_id: String,
        provider: Option<Provider>,
    ) -> anyhow::Result<Self> {
        Ok(UpgraderWallet::KMS(
            KMSWallet::from_kms_key_id(kms_key_id, provider).await?,
        ))
    }

    pub fn from_secret_key(private_key: SecretKey, provider: Option<Provider>) -> Self {
        UpgraderWallet::WalletUnlocked(WalletUnlocked::new_from_private_key(
            private_key,
            provider,
        ))
    }

    pub fn provider(&self) -> Option<&Provider> {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.provider(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.provider(),
        }
    }

    pub fn address(&self) -> &Bech32Address {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.address(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.address(),
        }
    }
}

impl ViewOnlyAccount for UpgraderWallet {
    fn address(&self) -> &Bech32Address {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.address(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.address(),
        }
    }

    fn try_provider(&self) -> Result<&Provider, Error> {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.try_provider(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.try_provider(),
        }
    }
}

#[async_trait::async_trait]
impl Account for UpgraderWallet {
    async fn get_asset_inputs_for_amount(
        &self,
        asset_id: AssetId,
        amount: u64,
        excluded_coins: Option<Vec<CoinTypeId>>,
    ) -> Result<Vec<Input>, Error> {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.get_asset_inputs_for_amount(asset_id, amount, excluded_coins).await,
            UpgraderWallet::WalletUnlocked(wallet) => wallet.get_asset_inputs_for_amount(asset_id, amount, excluded_coins).await
        }
    }

    fn add_witnesses<Tb: TransactionBuilder>(&self, tb: &mut Tb) -> Result<(), Error> {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.add_witnesses(tb),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.add_witnesses(tb),
        }
    }
}

#[async_trait::async_trait]
impl Signer for UpgraderWallet {
    async fn sign(&self, message: Message) -> Result<Signature, Error> {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.sign(message).await,
            UpgraderWallet::WalletUnlocked(wallet) => wallet.sign(message).await,
        }
    }

    fn address(&self) -> &Bech32Address {
        match self {
            UpgraderWallet::KMS(wallet) => wallet.address(),
            UpgraderWallet::WalletUnlocked(wallet) => wallet.address(),
        }
    }
}
