//! A simple utility tool to interact with the network.
use clap::Parser;
use fuel_tx::{
    Address,
    AssetId,
    Bytes32,
    ConsensusParameters,
    UploadSubsection,
};
use fuels::{
    accounts::Account,
    crypto::SecretKey,
    prelude::Provider,
    types::transaction_builders::{
            BuildableTransaction,
            ScriptTransactionBuilder,
            UpgradeTransactionBuilder,
            UploadTransactionBuilder,
        },
};
use std::{
    fs,
    path::PathBuf,
    time::Duration,
};
use termion::input::TermRead;
use upgrader_wallet::UpgraderWallet;

mod kms_wallet;
mod upgrader_wallet;

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

async fn create_wallet(
    aws_kms_key_id: Option<String>,
    provider: Option<Provider>,
) -> anyhow::Result<UpgraderWallet> {
    match aws_kms_key_id {
        Some(key_id) => UpgraderWallet::from_kms_key_id(key_id, provider).await,
        None => {
            println!("Paste the private key to sign the transaction:");
            let secret = std::io::stdin()
                .read_passwd(&mut std::io::stdout())?
                .ok_or(anyhow::anyhow!("The private key was not entered"))?;

            let secret_key: SecretKey = secret.as_str().parse()?;
            Ok(UpgraderWallet::from_secret_key(secret_key, provider))
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
    let wallet = create_wallet(upload.aws_kms_key_id.clone(), Some(provider)).await?;

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
        wallet.add_witnesses(&mut builder)?;
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
    let wallet = create_wallet(upgrade.aws_kms_key_id.clone(), Some(provider)).await?;

    match &upgrade.upgrade {
        UpgradeVariants::StateTransition(cmd) => {
            upgrade_state_transition(&wallet, cmd).await?;
        }
        UpgradeVariants::ConsensusParameters(cmd) => {
            upgrade_consensus_parameters(&wallet, cmd).await?;
        }
    }

    Ok(())
}

async fn upgrade_state_transition(
    wallet: &UpgraderWallet,
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
    wallet.add_witnesses(&mut builder)?;
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
    wallet: &UpgraderWallet,
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
    wallet.add_witnesses(&mut builder)?;
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
    let wallet = create_wallet(transfer.aws_kms_key_id.clone(), Some(provider)).await?;
    let recipient = transfer.recipient;
    let amount = transfer.amount;
    let asset_id = transfer
        .asset_id
        .unwrap_or(*consensus_parameters.base_asset_id());
    
    let sender: Address = wallet.address().into();
    let provider = wallet.provider().unwrap();

    let inputs = wallet
        .get_asset_inputs_for_amount(asset_id, amount, None)
        .await?;
    let outputs =
        wallet.get_asset_outputs_for_amount(&recipient.into(), asset_id, amount);

    let mut tx_builder =
        ScriptTransactionBuilder::prepare_transfer(inputs, outputs, Default::default());

    wallet.add_witnesses(&mut tx_builder)?;

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
