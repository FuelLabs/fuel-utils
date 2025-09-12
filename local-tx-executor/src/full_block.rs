use cynic::{
    Operation,
    QueryBuilder,
};
use fuel_core_client::client::{
    pagination::{
        PageDirection,
        PaginatedResult,
        PaginationRequest,
    },
    schema::{
        block::Header,
        schema,
        tx::OpaqueTransactionWithStatus,
        ConnectionArgs,
        ConnectionArgsFields,
        PageInfo,
    },
    types::{
        TransactionResponse,
        TransactionStatus,
    },
    FuelClient,
};
use fuel_core_types::{
    blockchain::{
        block::Block,
        header::{
            ApplicationHeader,
            ConsensusHeader,
            PartialBlockHeader,
        },
    },
    fuel_tx::Bytes32,
};
use itertools::Itertools;
use std::ops::Range;

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    schema_path = "./target/schema.sdl",
    graphql_type = "Query",
    variables = "ConnectionArgs"
)]
pub struct FullBlocksQuery {
    #[arguments(after: $after, before: $before, first: $first, last: $last)]
    pub blocks: FullBlockConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "BlockConnection")]
pub struct FullBlockConnection {
    pub edges: Vec<FullBlockEdge>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "BlockEdge")]
pub struct FullBlockEdge {
    pub cursor: String,
    pub node: FullBlock,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "Block")]
pub struct FullBlock {
    pub header: Header,
    pub transactions: Vec<OpaqueTransactionWithStatus>,
}

impl From<FullBlockConnection> for PaginatedResult<FullBlock, String> {
    fn from(conn: FullBlockConnection) -> Self {
        PaginatedResult {
            cursor: conn.page_info.end_cursor,
            has_next_page: conn.page_info.has_next_page,
            has_previous_page: conn.page_info.has_previous_page,
            results: conn.edges.into_iter().map(|e| e.node).collect(),
        }
    }
}

#[async_trait::async_trait]
pub trait ClientExt {
    async fn full_blocks(&self, range: Range<u32>) -> anyhow::Result<Vec<Block>>;
}

#[async_trait::async_trait]
impl ClientExt for FuelClient {
    async fn full_blocks(&self, range: Range<u32>) -> anyhow::Result<Vec<Block>> {
        let start = range.start.saturating_sub(1);
        let size = i32::try_from(range.len()).expect("Should be a valid i32");

        let request = PaginationRequest {
            cursor: Some(start.to_string()),
            results: size,
            direction: PageDirection::Forward,
        };

        let query: Operation<FullBlocksQuery, ConnectionArgs> =
            FullBlocksQuery::build(request.into());
        let blocks: FullBlocksQuery = self.query(query).await?;
        let blocks = blocks
            .blocks
            .edges
            .into_iter()
            .map(|b| try_from_full_block_and_chain_id(b.node))
            .try_collect()?;

        Ok(blocks)
    }
}

pub fn try_from_full_block_and_chain_id(full_block: FullBlock) -> anyhow::Result<Block> {
    let transactions: Vec<TransactionResponse> = full_block
        .transactions
        .into_iter()
        .map(TryInto::try_into)
        .try_collect()?;

    let receipts = transactions
        .iter()
        .map(|tx| &tx.status)
        .map(|status| match status {
            TransactionStatus::Success { receipts, .. } => Some(receipts.clone()),
            _ => None,
        })
        .collect_vec();

    let messages = receipts
        .iter()
        .flatten()
        .flat_map(|receipt| receipt.iter().filter_map(|r| r.message_id()))
        .collect_vec();

    let transactions: Vec<_> = transactions
        .into_iter()
        .map(|tx| tx.transaction.try_into())
        .try_collect()?;

    let partial_header = PartialBlockHeader {
        application: ApplicationHeader {
            da_height: full_block.header.da_height.0.into(),
            consensus_parameters_version: full_block
                .header
                .consensus_parameters_version
                .into(),
            state_transition_bytecode_version: full_block
                .header
                .state_transition_bytecode_version
                .into(),
            generated: Default::default(),
        },
        consensus: ConsensusHeader {
            prev_root: full_block.header.prev_root.into(),
            height: full_block.header.height.into(),
            time: full_block.header.time.into(),
            generated: Default::default(),
        },
    };

    let header = partial_header
        .generate(
            transactions.as_slice(),
            messages.as_slice(),
            full_block.header.event_inbox_root.into(),
        )
        .map_err(|e| anyhow::anyhow!(e))?;

    let actual_id: Bytes32 = full_block.header.id.into();
    let expected_id: Bytes32 = header.id().into();
    if expected_id != actual_id {
        return Err(anyhow::anyhow!("Header id mismatch"));
    }

    let block = Block::try_from_executed(header, transactions)
        .ok_or(anyhow::anyhow!("Failed to create block from transactions"))?;

    Ok(block)
}
