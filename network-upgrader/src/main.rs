//! A simple utility tool to interact with the network.
use clap::Parser;
use fuel_core_client::client::FuelClient;
use fuel_core_types::{
    fuel_crypto::Hasher,
    fuel_tx::{
        Address,
        AssetId,
        Bytes32,
        ConsensusParameters,
        UploadSubsection,
    },
};
use fuels::{
    accounts::Account,
    crypto::SecretKey,
    prelude::Provider,
    types::transaction_builders::{
        UpgradeTransactionBuilder,
        UploadTransactionBuilder,
    },
};
use fuels_core::types::{
    bech32::Bech32Address,
    transaction::Transaction,
    transaction_builders::TransactionBuilder,
    tx_status::TxStatus,
};
use indicatif::ProgressBar;
use std::{
    fs,
    path::PathBuf,
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

/// Fetches the current versions of the state transition bytecode and consensus parameters.
#[derive(Debug, clap::Args)]
pub struct CurrentVersions {
    /// The graphql API to the network to get the current versions of
    /// state transition bytecode and consensus parameters from.
    #[clap(long = "url", short = 'u', env)]
    pub url: String,
}

/// Utilities for interacting with the Fuel network.
#[derive(Debug, Parser)]
#[clap(name = "fuel-core-network-upgrader", author, version, about)]
enum Command {
    CurrentVersions(CurrentVersions),
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
            Command::CurrentVersions(cmd) => current_versions(cmd).await,
        }
    }
}

async fn upload(upload: &Upload) -> anyhow::Result<()> {
    let provider = Provider::connect(upload.url.as_str()).await?;

    let chain_id = provider.chain_id();
    let wallet = create_wallet(upload.aws_kms_key_id.clone(), Some(provider)).await?;

    println!("Reading bytecode from `{}`.", upload.path.to_string_lossy());
    let bytecode = fs::read(&upload.path)?;
    let subsections = UploadSubsection::split_bytecode(&bytecode, upload.subsection_size)
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let subsections_len = subsections.len();
    println!(
        "Split bytecode into `{}` subsections where size of each is `{}`. The root of the bytecode is `{}`.",
        subsections.len(),
        upload.subsection_size,
        subsections[0].root
    );
    let bar = ProgressBar::new(subsections_len as u64);

    for (i, subsection) in subsections
        .into_iter()
        .enumerate()
        .skip(upload.starting_subsection)
    {
        let provider = wallet.provider().unwrap();
        bar.println(format!(
            "Uploading subsection `{i}` of `{total}`.",
            i = i,
            total = subsections_len
        ));
        bar.inc(1);

        let subsection_witness_index = 0;
        let outputs = vec![];
        let UploadSubsection {
            root,
            subsection,
            subsection_index,
            subsections_number,
            proof_set,
        } = subsection;
        let witnesses = vec![subsection.into()];
        let proof_set = proof_set.into_iter().map(|p| (*p).into()).collect();

        let mut builder = UploadTransactionBuilder::default()
            .with_tx_policies(Default::default())
            .with_root((*root).into())
            .with_witness_index(subsection_witness_index)
            .with_subsection_index(subsection_index)
            .with_subsections_number(subsections_number)
            .with_proof_set(proof_set)
            .with_outputs(outputs)
            .with_max_fee_estimation_tolerance(2.0)
            .with_witnesses(witnesses);

        wallet.add_witnesses(&mut builder)?;
        wallet.adjust_for_fee(&mut builder, 0).await?;
        let tx = builder.build(provider).await?;
        let tx_id = tx.id(chain_id);

        let result = provider.send_transaction_and_await_commit(tx).await?;

        match result {
            TxStatus::Success { .. } => {
                println!(
                    "Subsection `{i}` successfully committed to the network with tx id `{tx_id}`."
                );
            }
            TxStatus::Submitted => {
                bar.abandon_with_message("an error was detected");
                anyhow::bail!(
                    "The transaction `{tx_id}` is submitted and awaiting confirmation."
                );
            }
            TxStatus::SqueezedOut { .. } => {
                bar.abandon_with_message("an error was detected");
                anyhow::bail!("Subsection `{i}` upload failed. The transaction `{tx_id}` was squeezed out.");
            }
            TxStatus::Revert { reason, .. } => {
                bar.abandon_with_message("an error was detected");
                anyhow::bail!("Subsection `{i}` upload failed. The transaction `{tx_id}` was reverted with reason `{reason}`.");
            }
        }
    }

    bar.finish_with_message("All subsections are successfully uploaded.");

    Ok(())
}

async fn upgrade(upgrade: &Upgrade) -> anyhow::Result<()> {
    current_versions_from_url(upgrade.url.as_str()).await?;

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

    current_versions_from_url(upgrade.url.as_str()).await?;

    Ok(())
}

async fn upgrade_state_transition(
    wallet: &UpgraderWallet,
    state_transition: &StateTransition,
) -> anyhow::Result<()> {
    let provider = wallet
        .provider()
        .ok_or(anyhow::anyhow!("Provider is not set."))?;

    let chain_id = provider.chain_id();
    let root = state_transition.root;
    println!(
        "Preparing upgrade of state transition function with the root `{}`.",
        root
    );
    let mut builder = UpgradeTransactionBuilder::prepare_state_transition_upgrade(
        (*root).into(),
        Default::default(),
    )
    .with_max_fee_estimation_tolerance(2.0)
    .with_estimation_horizon(10);
    wallet.add_witnesses(&mut builder)?;
    wallet.adjust_for_fee(&mut builder, 0).await?;
    let tx = builder.build(provider).await?;
    let tx_id = tx.id(chain_id);

    let result = provider.send_transaction_and_await_commit(tx).await?;

    match result {
        TxStatus::Success { .. } => {
            println!(
                "The state transition function of the network \
                is successfully upgraded to `{root}` by transaction `{tx_id}`."
            );
        }
        TxStatus::Submitted => {
            anyhow::bail!(
                "The transaction `{tx_id}` is submitted and awaiting confirmation."
            );
        }
        TxStatus::SqueezedOut { .. } => {
            anyhow::bail!("The transaction `{tx_id}` was squeezed out.");
        }
        TxStatus::Revert { reason, .. } => {
            anyhow::bail!(
                "The transaction `{tx_id}` was reverted with reason `{reason}`."
            );
        }
    }

    Ok(())
}

async fn upgrade_consensus_parameters(
    wallet: &UpgraderWallet,
    cmd: &ConsensusParametersCommand,
) -> anyhow::Result<()> {
    let provider = wallet
        .provider()
        .ok_or(anyhow::anyhow!("Provider is not set."))?;

    let chain_id = provider.chain_id();
    println!(
        "Preparing upgrade of consensus parameters from `{}` file.",
        cmd.path.to_string_lossy()
    );
    let consensus_parameters = fs::read_to_string(&cmd.path)?;
    let new_consensus_parameters: ConsensusParameters =
        serde_json::from_str(consensus_parameters.as_str())?;

    let serialized_consensus_parameters =
        postcard::to_allocvec(&new_consensus_parameters)
            .expect("Impossible to fail unless there is not enough memory");

    let deserialize =
        postcard::from_bytes::<ConsensusParameters>(&serialized_consensus_parameters)?;
    assert_eq!(deserialize, new_consensus_parameters);

    let checksum = Hasher::hash(&serialized_consensus_parameters);
    let witness_index = 0;
    let outputs = vec![];
    let witnesses = vec![serialized_consensus_parameters.into()];

    let mut builder = UpgradeTransactionBuilder::default()
        .with_tx_policies(Default::default())
        .with_purpose(fuels::tx::UpgradePurpose::ConsensusParameters {
            witness_index,
            checksum: (*checksum).into(),
        })
        .with_outputs(outputs)
        .with_witnesses(witnesses)
        .with_estimation_horizon(10);
    wallet.add_witnesses(&mut builder)?;
    wallet.adjust_for_fee(&mut builder, 0).await?;
    let tx = builder.build(provider).await?;
    let tx_id = tx.id(chain_id);

    let result = provider.send_transaction_and_await_commit(tx).await?;

    match result {
        TxStatus::Success { .. } => {
            println!(
                "The consensus parameters of the network \
                are successfully upgraded by transaction `{tx_id}`."
            );
        }
        TxStatus::Submitted => {
            anyhow::bail!(
                "The transaction `{tx_id}` is submitted and awaiting confirmation."
            );
        }
        TxStatus::SqueezedOut { .. } => {
            anyhow::bail!("The transaction `{tx_id}` was squeezed out.");
        }
        TxStatus::Revert { reason, .. } => {
            anyhow::bail!(
                "The transaction `{tx_id}` was reverted with reason `{reason}`."
            );
        }
    }

    Ok(())
}

async fn transfer(transfer: &Transfer) -> anyhow::Result<()> {
    let provider = Provider::connect(transfer.url.as_str()).await?;
    let consensus_parameters = provider.consensus_parameters().clone();
    let wallet = create_wallet(transfer.aws_kms_key_id.clone(), Some(provider)).await?;
    let recipient: fuels::types::Address = (*transfer.recipient).into();
    let bench_recipient = Bech32Address::from(recipient);
    let amount = transfer.amount;
    let asset_id = transfer.asset_id.map(|id| (*id).into());
    let asset_id = asset_id.unwrap_or(*consensus_parameters.base_asset_id());
    let sender = wallet.address();

    wallet
        .transfer(&bench_recipient, amount, asset_id, Default::default())
        .await?;

    println!(
        "Successfully transferred `{amount}` of `{asset_id}` \
        assets to recipient `{recipient}`, from sender `{sender}`."
    );

    Ok(())
}

async fn parameters(parameters: &Parameters) -> anyhow::Result<()> {
    let client = FuelClient::new(parameters.url.as_str())?;
    let consensus_parameters = client.chain_info().await?.consensus_parameters;
    let json = serde_json::to_string_pretty(&consensus_parameters)?;
    println!("Writing file into `{}`.", parameters.path.to_string_lossy());
    fs::write(&parameters.path, json)?;
    Ok(())
}

async fn current_versions_from_url(url: &str) -> anyhow::Result<()> {
    let client = FuelClient::new(url)?;
    let chain_info = client.chain_info().await?;
    let stf_version = chain_info
        .latest_block
        .header
        .state_transition_bytecode_version;
    let consensus_params_version =
        chain_info.latest_block.header.consensus_parameters_version;
    println!(
        "The current state transition bytecode version is `{stf_version}` \
        and the consensus parameters version is `{consensus_params_version}`."
    );
    Ok(())
}

async fn current_versions(current_versions: &CurrentVersions) -> anyhow::Result<()> {
    current_versions_from_url(&current_versions.url).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = Command::parse();
    cmd.exec().await
}
