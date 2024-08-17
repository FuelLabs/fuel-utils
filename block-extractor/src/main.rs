//! A simple utility tool to interact with the network.

use clap::Parser;
use fuel_core_client::client::{
    pagination::{
        PageDirection,
        PaginationRequest,
    },
    FuelClient,
};
use fuel_core_client_ext::{
    ClientExt,
    FullBlock,
};
use fuel_core_types::{
    blockchain::{
        block::Block,
        header::{
            ApplicationHeader,
            BlockHeader,
            ConsensusHeader,
            GeneratedApplicationFields,
            GeneratedConsensusFields,
        },
    },
    fuel_types::canonical::Deserialize,
};
use fuel_tx::Transaction;
use std::{
    fs,
    path::PathBuf,
};

/// Fetch the blocks starting specific block height.
#[derive(Debug, clap::Args)]
pub struct Blocks {
    /// The URL to fetch blocks from.
    #[clap(long = "url", short = 'u', env)]
    pub url: String,
    /// The starting block height.
    #[clap(long = "starting-block-height", short = 'h', env)]
    pub starting_block_height: u32,
    /// The directory where to store the blocks.
    #[clap(long = "output-directory", short = 'o', env)]
    pub output_directory: PathBuf,
}

impl Blocks {
    fn block_path(&self, block_height: u32) -> PathBuf {
        self.output_directory.join(format!("{}.json", block_height))
    }

    fn block_exists(&self, block_height: u32) -> bool {
        self.block_path(block_height).exists()
    }
}

/// Utilities for interacting with the Fuel network.
#[derive(Debug, Parser)]
#[clap(name = "fuel-core-block-extractor", author, version, about)]
enum Command {
    Blocks(Blocks),
}

impl Command {
    async fn exec(&self) -> anyhow::Result<()> {
        match self {
            Command::Blocks(cmd) => blocks(cmd).await,
        }
    }
}

async fn blocks(cmd: &Blocks) -> anyhow::Result<()> {
    const BATCH_SIZE: u32 = 1000;

    fs::create_dir_all(&cmd.output_directory)?;

    let client = FuelClient::new(cmd.url.as_str())?;
    let last_block_height = client.chain_info().await?.latest_block.header.height;

    let mut starting_block_height = cmd.starting_block_height;

    while starting_block_height <= last_block_height {
        if cmd.block_exists(starting_block_height) {
            println!("Block `{}` already exists.", starting_block_height);
            starting_block_height += 1;
            continue;
        }

        let starting_block_height_request = starting_block_height - 1;
        let page = PaginationRequest {
            cursor: Some(format!("{starting_block_height_request}")),
            results: BATCH_SIZE as i32,
            direction: PageDirection::Forward,
        };
        let blocks = client.full_blocks(page).await?;

        let len = blocks.results.len() as u32;
        for block in blocks.results {
            let height = block.header.height.0;
            let fuel_block = convert_to_fuel_block(block).ok_or(anyhow::anyhow!(
                "Failed to convert block at height `{}`.",
                height
            ))?;

            let height = *fuel_block.header().height();
            let height: u32 = height.into();

            if !cmd.block_exists(height) {
                let path = cmd.block_path(height);
                fs::write(path, serde_json::to_string_pretty(&fuel_block)?)?;
            }
        }

        println!(
            "Fetched batch in a range {}..{}",
            starting_block_height,
            starting_block_height + len
        );
        starting_block_height = starting_block_height + len;
    }

    Ok(())
}

fn convert_to_fuel_block(full_block: FullBlock) -> Option<Block> {
    let application_header = ApplicationHeader {
        da_height: full_block.header.da_height.0.into(),
        consensus_parameters_version: full_block
            .header
            .consensus_parameters_version
            .into(),
        state_transition_bytecode_version: full_block
            .header
            .state_transition_bytecode_version
            .into(),
        generated: GeneratedApplicationFields {
            transactions_count: full_block.header.transactions_count.into(),
            message_receipt_count: full_block.header.message_receipt_count.into(),
            transactions_root: full_block.header.transactions_root.into(),
            message_outbox_root: full_block.header.message_outbox_root.into(),
            event_inbox_root: full_block.header.event_inbox_root.into(),
        },
    };
    let consensus_header = ConsensusHeader {
        prev_root: full_block.header.prev_root.into(),
        height: full_block.header.height.into(),
        time: full_block.header.time.0,
        generated: GeneratedConsensusFields {
            application_hash: application_header.hash(),
        },
    };
    let mut header = BlockHeader::default();
    header.set_consensus_header(consensus_header);
    header.set_application_header(application_header);

    let mut transactions = Vec::with_capacity(full_block.transactions.len());

    for tx in full_block.transactions.iter() {
        let bytes = &tx.raw_payload.0 .0;
        let tx: Transaction = Transaction::from_bytes(bytes.as_slice()).ok()?;
        transactions.push(tx);
    }

    Block::try_from_executed(header, transactions)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = Command::parse();
    cmd.exec().await
}
