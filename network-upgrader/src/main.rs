//! A simple utility tool to interact with the network.
use anyhow::anyhow;
use async_trait::async_trait;
use aws_sdk_kms::{
    primitives::Blob,
    types::{
        MessageType,
        SigningAlgorithmSpec,
    },
};
use clap::Parser;
use fuel_tx::{
    Address,
    AssetId,
    Bytes32,
    ConsensusParameters,
    UploadSubsection,
};
use fuels::{
    accounts::{
        wallet::Wallet,
        ViewOnlyAccount,
    },
    core::traits::Signer,
    crypto::{
        Message,
        PublicKey,
        SecretKey,
        Signature,
    },
    prelude::Provider,
    types::{
        bech32::{
            Bech32Address,
            FUEL_BECH32_HRP,
        },
        transaction_builders::{
            BuildableTransaction,
            ScriptTransactionBuilder,
            TransactionBuilder,
            UpgradeTransactionBuilder,
            UploadTransactionBuilder,
        },
    },
};
use k256::pkcs8::DecodePublicKey;
use std::{
    fs,
    path::PathBuf,
    time::Duration,
};
use termion::input::TermRead;

/// Uploads the state transition bytecode.
#[derive(Debug, clap::Args)]
pub struct Upload {
    /// The path to the bytecode file to upload.
    #[clap(long = "path", short = 'p')]
    pub path: PathBuf,
    /// The URL to upload the bytecode to.
    #[clap(long = "url", short = 'u', env)]
    pub url: String,
    /// The size of the subsections to split the bytecode into.
    /// Default size is 64KB.
    #[clap(long = "subsection-size", default_value = "65536", env)]
    pub subsection_size: usize,
    /// The size of the subsections to split the bytecode into.
    /// Default size is 64KB.
    #[clap(long = "starting-subsection", default_value = "0", short = 's', env)]
    pub starting_subsection: usize,
    /// Optional flag to sign with KMS
    #[clap(long = "aws_kms_key_id", default_value = None, env)]
    pub aws_kms_key_id: Option<String>,
}

/// Transfers assets to recipient.
#[derive(Debug, clap::Args)]
pub struct Transfer {
    /// The amount to transfer.
    #[clap(long = "amount", short = 'a', env)]
    pub amount: u64,
    /// The recipient of the transfer.
    #[clap(long = "recipient", short = 'r', env)]
    pub recipient: Address,
    /// The URL to upload the bytecode to.
    #[clap(long = "url", short = 'u', env)]
    pub url: String,
    /// The asset to transfer.
    #[clap(long = "asset-id", short = 'i', env)]
    pub asset_id: Option<AssetId>,
    /// Optional flag to sign with KMS
    #[clap(long = "aws_kms_key_id", default_value = None, env)]
    pub aws_kms_key_id: Option<String>,
}

/// Upgrades the state transition function of the network.
#[derive(Debug, clap::Args)]
pub struct Upgrade {
    /// The URL to upload the bytecode to.
    #[clap(long = "url", short = 'u', env)]
    pub url: String,
    #[clap(subcommand)]
    upgrade: UpgradeVariants,
    /// Optional flag to sign with KMS
    #[clap(long = "aws_kms_key_id", default_value = None, env)]
    pub aws_kms_key_id: Option<String>,
}

/// The command allows the upgrade of the Fuel network.
#[derive(Debug, clap::Subcommand)]
enum UpgradeVariants {
    StateTransition(StateTransition),
    ConsensusParameters(ConsensusParametersCommand),
}

/// Upgrades the state transition function of the network.
#[derive(Debug, clap::Args)]
pub struct StateTransition {
    /// The root of the bytecode of the state transition function.
    #[clap(long = "root", short = 'r')]
    pub root: Bytes32,
}

/// Upgrades the consensus parameters of the network.
#[derive(Debug, clap::Args)]
pub struct ConsensusParametersCommand {
    /// The path to the consensus parameters file. JSON format is used to decode the file.
    #[clap(long = "path", short = 'p')]
    pub path: PathBuf,
}

/// Downloads consensus parameters from the node.
#[derive(Debug, clap::Args)]
pub struct Parameters {
    /// The path to dump the consensus parameters.
    #[clap(
        long = "path",
        short = 'p',
        default_value = "consensus_parameters.json"
    )]
    pub path: PathBuf,
    /// The URL to upload the bytecode to.
    #[clap(long = "url", short = 'u', env)]
    pub url: String,
}

/// Utilities for interacting with the Fuel network.
#[derive(Debug, Parser)]
#[clap(name = "fuel-core-network-upgrader", author, version, about)]
enum Command {
    Upgrade(Upgrade),
    Upload(Upload),
    Transfer(Transfer),
    Parameters(Parameters),
}

#[derive(Clone)]
pub enum SignerData {
    Kms {
        key_id: String,
        client: aws_sdk_kms::Client,
        cached_public_key: Vec<u8>,
        cached_address: Bech32Address,
    },
    SecretKey {
        secret_key: SecretKey,
        cached_address: Bech32Address,
    },
}

impl SignerData {
    fn get_address(&self) -> anyhow::Result<&Bech32Address, anyhow::Error> {
        match self {
            SignerData::Kms { cached_address, .. } => Ok(cached_address),
            SignerData::SecretKey { cached_address, .. } => Ok(cached_address),
        }
    }
}

#[async_trait]
impl Signer for SignerData {
    fn address(&self) -> &Bech32Address {
        self.get_address().unwrap()
    }

    async fn sign(
        &self,
        message: Message,
    ) -> Result<Signature, fuels::types::errors::Error> {
        match self {
            SignerData::Kms {
                key_id,
                client,
                cached_public_key,
                ..
            } => sign_with_kms(client, key_id, cached_public_key, message)
                .await
                .map_err(|e| {
                    fuels::types::errors::Error::Other(format!(
                        "Failed to sign with AWS KMS: {:?}",
                        e
                    ))
                }),
            SignerData::SecretKey { secret_key, .. } => {
                let sig = Signature::sign(&secret_key, &message);
                Ok(sig)
            }
        }
    }
}

async fn sign_with_kms(
    client: &aws_sdk_kms::Client,
    key_id: &str,
    public_key_bytes: &[u8],
    message: Message,
) -> anyhow::Result<Signature> {
    use k256::{
        ecdsa::{
            RecoveryId,
            VerifyingKey,
        },
        pkcs8::DecodePublicKey,
    };

    let reply = client
        .sign()
        .key_id(key_id)
        .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
        .message_type(MessageType::Digest)
        .message(Blob::new(*message))
        .send()
        .await?;
    let signature_der = reply
        .signature
        .ok_or_else(|| anyhow!("no signature returned from AWS KMS"))?
        .into_inner();
    // https://stackoverflow.com/a/71475108
    let sig = k256::ecdsa::Signature::from_der(&signature_der)
        .map_err(|_| anyhow!("invalid DER signature from AWS KMS"))?;
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

    let correct_public_key = k256::PublicKey::from_public_key_der(public_key_bytes)
        .map_err(|_| anyhow!("invalid DER public key from AWS KMS"))?
        .into();

    let recovery_id = if rec1.map(|r| r == correct_public_key).unwrap_or(false) {
        recid1
    } else if rec2.map(|r| r == correct_public_key).unwrap_or(false) {
        recid2
    } else {
        anyhow::bail!("Invalid signature generated (reduced-x form coordinate)");
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

async fn handle_secret(aws_kms_key_id: Option<String>) -> anyhow::Result<SignerData> {
    match aws_kms_key_id {
        Some(key_id) => {
            let config = aws_config::load_from_env().await;
            let client = aws_sdk_kms::Client::new(&config);
            // Ensure that the key is accessible and has the correct type
            let key = client
                .get_public_key()
                .key_id(&key_id)
                .send()
                .await?
                .key_spec;
            if key != Some(aws_sdk_kms::types::KeySpec::EccSecgP256K1) {
                anyhow::bail!("The key is not of the correct type, got {:?}", key);
            }
            let Ok(key) = client.get_public_key().key_id(key_id.clone()).send().await
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
            let hashed = public_key.hash();
            let address = Bech32Address::new(FUEL_BECH32_HRP, hashed);
            Ok(SignerData::Kms {
                key_id,
                client,
                cached_public_key: public_key_bytes,
                cached_address: address,
            })
        }
        None => {
            println!("Paste the private key to sign the transaction:");
            let secret = std::io::stdin()
                .read_passwd(&mut std::io::stdout())?
                .ok_or(anyhow::anyhow!("The private key was not entered"))?;

            let secret_key: SecretKey = secret.as_str().parse()?;
            let public = PublicKey::from(&secret_key);
            let hashed = public.hash();
            let address = Bech32Address::new(FUEL_BECH32_HRP, hashed);
            println!("The private key was decoded successfully.");
            Ok(SignerData::SecretKey {
                secret_key,
                cached_address: address,
            })
        }
    }
}

impl Command {
    async fn exec(&self) -> anyhow::Result<()> {
        match self {
            Command::Upgrade(cmd) => upgrade(cmd).await,
            Command::Upload(cmd) => upload(cmd).await,
            Command::Transfer(cmd) => transfer(cmd).await,
            Command::Parameters(cmd) => parameters(cmd).await,
        }
    }
}

async fn upload(upload: &Upload) -> anyhow::Result<()> {
    let provider = Provider::connect(upload.url.as_str()).await?;
    let signer = handle_secret(upload.aws_kms_key_id.clone()).await?;
    let wallet = Wallet::from_address(signer.get_address()?.into(), Some(provider));

    println!("Reading bytecode from `{}`.", upload.path.to_string_lossy());
    let bytecode = fs::read(&upload.path)?;
    let subsections = UploadSubsection::split_bytecode(&bytecode, upload.subsection_size)
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    println!(
        "Split bytecode into `{}` subsections where size of each is `{}`. The root of the bytecode is `{}`.",
        subsections.len(),
        upload.subsection_size,
        subsections[0].root
    );

    for (i, subsection) in subsections
        .into_iter()
        .enumerate()
        .skip(upload.starting_subsection)
    {
        let provider = wallet.provider().unwrap();
        println!("Processing subsection `{i}`.");
        let mut builder = UploadTransactionBuilder::prepare_subsection_upload(
            subsection,
            Default::default(),
        );
        builder.add_signer(signer.clone())?;
        wallet.adjust_for_fee(&mut builder, 0).await?;
        let max_fee = builder.tx_policies.max_fee().unwrap_or(1_000_000_000);
        builder.tx_policies = builder.tx_policies.with_max_fee(max_fee * 2);
        let tx = builder.build(provider).await?;

        let result = provider.send_transaction(tx).await?;
        println!("Subsection `{i}` successfully committed to the network with tx id `{result}`.");
        // Wait for sentries to update off-chain database.
        tokio::time::sleep(Duration::from_secs(4)).await;
    }

    Ok(())
}

async fn upgrade(upgrade: &Upgrade) -> anyhow::Result<()> {
    let provider = Provider::connect(upgrade.url.as_str()).await?;
    let signer = handle_secret(upgrade.aws_kms_key_id.clone()).await?;
    let wallet = Wallet::from_address(signer.get_address()?.into(), Some(provider));

    match &upgrade.upgrade {
        UpgradeVariants::StateTransition(cmd) => {
            upgrade_state_transition(&wallet, signer, cmd).await?;
        }
        UpgradeVariants::ConsensusParameters(cmd) => {
            upgrade_consensus_parameters(&wallet, signer, cmd).await?;
        }
    }

    Ok(())
}

async fn upgrade_state_transition(
    wallet: &Wallet,
    signer: SignerData,
    state_transition: &StateTransition,
) -> anyhow::Result<()> {
    let provider = wallet.provider().unwrap();
    let root = state_transition.root;
    println!(
        "Preparing upgrade of state transition function with the root `{}`.",
        root
    );
    let mut builder = UpgradeTransactionBuilder::prepare_state_transition_upgrade(
        root,
        Default::default(),
    );
    builder.add_signer(signer)?;
    wallet.adjust_for_fee(&mut builder, 0).await?;
    let max_fee = builder.tx_policies.max_fee().unwrap_or(1_000_000_000);
    builder.tx_policies = builder.tx_policies.with_max_fee(max_fee * 2);
    let tx = builder.build(provider).await?;

    let result = provider.send_transaction(tx).await?;
    println!(
        "The state transition function of the network \
        is successfully upgraded to `{root}` by transaction `{result}`."
    );

    Ok(())
}

async fn upgrade_consensus_parameters(
    wallet: &Wallet,
    signer: SignerData,
    cmd: &ConsensusParametersCommand,
) -> anyhow::Result<()> {
    let provider = wallet.provider().unwrap();
    println!(
        "Preparing upgrade of consensus parameters from `{}` file.",
        cmd.path.to_string_lossy()
    );
    let consensus_parameters = fs::read(&cmd.path)?;
    let new_consensus_parameters: ConsensusParameters =
        serde_json::from_slice(consensus_parameters.as_slice())?;

    let mut builder = UpgradeTransactionBuilder::prepare_consensus_parameters_upgrade(
        &new_consensus_parameters,
        Default::default(),
    );
    builder.add_signer(signer)?;
    wallet.adjust_for_fee(&mut builder, 0).await?;
    let max_fee = builder.tx_policies.max_fee().unwrap_or(1_000_000_000);
    builder.tx_policies = builder.tx_policies.with_max_fee(max_fee * 2);
    let tx = builder.build(provider).await?;

    let result = provider.send_transaction(tx).await?;
    println!(
        "The consensus parameters of the network \
        are successfully upgraded by transaction `{result}`."
    );

    Ok(())
}

async fn transfer(transfer: &Transfer) -> anyhow::Result<()> {
    let provider = Provider::connect(transfer.url.as_str()).await?;
    let consensus_parameters = provider.consensus_parameters().clone();
    let signer = handle_secret(transfer.aws_kms_key_id.clone()).await?;
    let wallet =
        Wallet::from_address(signer.get_address()?.into(), Some(provider.clone()));
    let recipient = transfer.recipient;
    let amount = transfer.amount;
    let asset_id = transfer
        .asset_id
        .unwrap_or(*consensus_parameters.base_asset_id());
    let sender: Address = wallet.address().into();

    let inputs = wallet
        .get_asset_inputs_for_amount(asset_id, amount, None)
        .await?;
    let outputs =
        wallet.get_asset_outputs_for_amount(&recipient.into(), asset_id, amount);

    let mut tx_builder =
        ScriptTransactionBuilder::prepare_transfer(inputs, outputs, Default::default());

    tx_builder.add_signer(signer.clone())?;

    let used_base_amount = if asset_id == *provider.base_asset_id() {
        amount
    } else {
        0
    };
    wallet
        .adjust_for_fee(&mut tx_builder, used_base_amount)
        .await?;

    let tx = tx_builder.build(provider.clone()).await?;

    provider.send_transaction_and_await_commit(tx).await?;

    println!(
        "Successfully transferred `{amount}` of `{asset_id}` \
        assets to recipient `{recipient}`, from sender `{sender}`."
    );

    Ok(())
}

async fn parameters(parameters: &Parameters) -> anyhow::Result<()> {
    let provider = Provider::connect(parameters.url.as_str()).await?;
    let json = serde_json::to_string_pretty(provider.consensus_parameters())?;
    println!("Writing file into `{}`.", parameters.path.to_string_lossy());
    fs::write(&parameters.path, json)?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = Command::parse();
    cmd.exec().await
}
