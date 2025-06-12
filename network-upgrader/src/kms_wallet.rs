use aws_sdk_kms::{
    primitives::Blob,
    types::{
        MessageType,
        SigningAlgorithmSpec,
    },
};
use fuels::{
    accounts::{
        Account,
        ViewOnlyAccount,
        provider::Provider,
        signers::kms::{
            self,
            aws::AwsKmsSigner,
        },
        wallet::{
            Unlocked,
            Wallet,
        },
    },
    crypto::{
        Message,
        PublicKey,
        Signature,
    },
    types::{
        Address,
        AssetId,
        coin_type_id::CoinTypeId,
        errors::Error,
        input::Input,
        transaction_builders::TransactionBuilder,
    },
};
use fuels_core::traits::Signer;
use k256::{
    ecdsa::{
        RecoveryId,
        VerifyingKey,
    },
    pkcs8::DecodePublicKey,
};

#[derive(Clone, Debug)]
pub struct KMSWallet {
    pub wallet: Wallet<Unlocked<AwsKmsSigner>>,
    kms_data: KMSData,
}

#[derive(Clone, Debug)]
pub struct KMSData {
    key_id: String,
    client: aws_sdk_kms::Client,
    cached_public_key: Vec<u8>,
    cached_address: Address,
}

impl KMSWallet {
    // Requires "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_ENDPOINT_URL" and "AWS_REGION" to be defined in env.
    pub async fn from_kms_key_id(
        kms_key_id: String,
        provider: Provider,
    ) -> anyhow::Result<Self> {
        let config = aws_config::load_from_env().await;
        let client = aws_sdk_kms::Client::new(&config);
        // Ensure that the key is accessible and has the correct type
        let key = client
            .get_public_key()
            .key_id(&kms_key_id)
            .send()
            .await?
            .key_spec;
        if key != Some(aws_sdk_kms::types::KeySpec::EccSecgP256K1) {
            anyhow::bail!("The key is not of the correct type, got {:?}", key);
        }
        let Ok(key) = client
            .get_public_key()
            .key_id(kms_key_id.clone())
            .send()
            .await
        else {
            anyhow::bail!("The AWS KMS key is not accessible");
        };
        let Some(aws_kms_public_key) = key.public_key() else {
            anyhow::bail!("AWS KMS did not return a public key when requested");
        };
        let public_key_bytes = aws_kms_public_key.clone().into_inner();
        let Ok(correct_public_key) =
            k256::PublicKey::from_public_key_der(&public_key_bytes)
        else {
            anyhow::bail!("invalid DER public key from AWS KMS");
        };
        let public_key = PublicKey::from(correct_public_key);
        let fuel_address = public_key.hash();

        println!("Fuel address: {}", fuel_address);

        let address: Address = (*fuel_address).into();
        let kms_signer = kms::aws::AwsKmsSigner::new(&kms_key_id, &client).await?;
        let wallet = Wallet::new(kms_signer, provider);

        Ok(Self {
            wallet,
            kms_data: KMSData {
                key_id: kms_key_id,
                client,
                cached_public_key: public_key_bytes,
                cached_address: address,
            },
        })
    }

    pub fn address(&self) -> Address {
        self.wallet.address()
    }

    pub fn provider(&self) -> &Provider {
        self.wallet.provider()
    }
}

#[async_trait::async_trait]
impl Signer for KMSWallet {
    fn address(&self) -> Address {
        self.kms_data.cached_address
    }

    async fn sign(&self, message: Message) -> Result<Signature, Error> {
        let reply = self
            .kms_data
            .client
            .sign()
            .key_id(&self.kms_data.key_id)
            .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
            .message_type(MessageType::Digest)
            .message(Blob::new(*message))
            .send()
            .await
            .map_err(|e| Error::Other(format!("Failed to sign with AWS KMS: {:?}", e)))?;
        let signature_der = reply
            .signature
            .ok_or_else(|| Error::Other("no signature returned from AWS KMS".to_owned()))?
            .into_inner();
        // https://stackoverflow.com/a/71475108
        let sig = k256::ecdsa::Signature::from_der(&signature_der)
            .map_err(|_| Error::Other("no signature returned from AWS KMS".to_owned()))?;
        let sig = sig.normalize_s().unwrap_or(sig);

        // This is a hack to get the recovery id. The signature should be normalized
        // before computing the recovery id, but aws kms doesn't support this, and
        // instead always computes the recovery id from non-normalized signature.
        // So instead the recovery id is determined by checking which variant matches
        // the original public key.

        let recid1 = RecoveryId::new(false, false);
        let recid2 = RecoveryId::new(true, false);

        let rec1 = VerifyingKey::recover_from_prehash(&*message, &sig, recid1);
        let rec2 = VerifyingKey::recover_from_prehash(&*message, &sig, recid2);

        let correct_public_key =
            k256::PublicKey::from_public_key_der(&self.kms_data.cached_public_key)
                .map_err(|_| {
                    Error::Other("invalid DER signature from AWS KMS".to_owned())
                })?
                .into();

        let recovery_id = if rec1.map(|r| r == correct_public_key).unwrap_or(false) {
            recid1
        } else if rec2.map(|r| r == correct_public_key).unwrap_or(false) {
            recid2
        } else {
            return Err(Error::Other(
                "Invalid signature generated (reduced-x form coordinate)".to_owned(),
            ));
        };

        // Insert the recovery id into the signature
        debug_assert!(
            !recovery_id.is_x_reduced(),
            "reduced-x form coordinates are caught by the if-else chain above"
        );
        let v = recovery_id.is_y_odd() as u8;
        let mut signature = <[u8; 64]>::from(sig.to_bytes());
        signature[32] = (v << 7) | (signature[32] & 0x7f);
        Ok(Signature::from_bytes(signature))
    }
}

// Same implementation as `WalletUnlocked` of Rust SDK
#[async_trait::async_trait]
impl ViewOnlyAccount for KMSWallet {
    async fn get_asset_inputs_for_amount(
        &self,
        asset_id: AssetId,
        amount: u128,
        excluded_coins: Option<Vec<CoinTypeId>>,
    ) -> Result<Vec<Input>, Error> {
        Ok(self
            .get_spendable_resources(asset_id, amount, excluded_coins)
            .await?
            .into_iter()
            .map(Input::resource_signed)
            .collect::<Vec<Input>>())
    }

    fn address(&self) -> Address {
        self.wallet.address()
    }

    fn try_provider(&self) -> Result<&Provider, Error> {
        Ok(self.wallet.provider())
    }
}

// Same implementation as `WalletUnlocked` of Rust SDK
#[async_trait::async_trait]
impl Account for KMSWallet {
    fn add_witnesses<Tb: TransactionBuilder>(&self, tb: &mut Tb) -> Result<(), Error> {
        tb.add_signer(self.clone())?;

        Ok(())
    }
}
