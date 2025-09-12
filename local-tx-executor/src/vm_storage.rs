use crate::{
    contract_state_loader::ContractStateLoader,
    contract_storage::ContractStorage,
    vm_execution_storage::ExecutionVMStorage,
};
use fuel_core_client::client::FuelClient;
use fuel_core_types::blockchain::{
    block::Block,
    header::ConsensusParametersVersion,
};
use fuel_tx::{
    field::{
        Inputs,
        ReceiptsRoot,
    },
    ConsensusParameters,
    Contract,
    Receipt,
    Transaction,
    UniqueIdentifier,
};
use fuel_types::{
    BlobId,
    BlockHeight,
    Bytes32,
    ContractId,
    Word,
};
use fuel_vm::{
    checked_transaction::IntoChecked,
    interpreter::{
        Interpreter,
        InterpreterParams,
        MemoryInstance,
        NotSupportedEcal,
    },
    storage::{
        BlobBytes,
        ContractsAssetKey,
        ContractsStateData,
        ContractsStateKey,
    },
};
use futures::FutureExt;
use std::{
    collections::HashMap,
    future::Future,
    ops::Deref,
    path::PathBuf,
    sync::Arc,
};
use tokio::runtime::Handle;

fn default_client() -> Arc<FuelClient> {
    Arc::new(FuelClient::new("http://localhost:4000").unwrap())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct VMStorage {
    #[serde(skip)]
    #[serde(default = "default_client")]
    pub(crate) client: Arc<FuelClient>,
    pub(crate) last_block: Block,
    pub(crate) contracts_storage: HashMap<ContractId, ContractStorage>,
    pub(crate) contracts_bytecodes: HashMap<ContractId, Option<Contract>>,
    pub(crate) blobs: HashMap<BlobId, Option<BlobBytes>>,
    pub(crate) headers: HashMap<BlockHeight, (Word, Bytes32)>,
    pub(crate) consensus_parameters:
        HashMap<ConsensusParametersVersion, Arc<ConsensusParameters>>,
}

impl VMStorage {
    pub fn new(client: Arc<FuelClient>, last_block: Block) -> Self {
        Self {
            client,
            last_block,
            contracts_storage: HashMap::new(),
            contracts_bytecodes: HashMap::new(),
            blobs: HashMap::new(),
            headers: HashMap::new(),
            consensus_parameters: HashMap::new(),
        }
    }

    pub fn from_storage(client: Arc<FuelClient>, path: PathBuf) -> anyhow::Result<Self> {
        let storage = std::fs::read_to_string(path)?;
        let mut storage: VMStorage = serde_json::from_str(&storage)?;
        storage.client = client;

        Ok(storage)
    }

    pub fn to_storage(&self, path: &PathBuf) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(&self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn block_height(&self) -> BlockHeight {
        *self.last_block.header().height()
    }

    pub fn header(
        &mut self,
        block_height: BlockHeight,
    ) -> anyhow::Result<(Word, Bytes32)> {
        let result = self.header_inner(block_height).now_or_never();

        match result {
            None => Self::block_on(self.header_inner(block_height)),
            Some(result) => return result,
        }
    }

    async fn header_inner(
        &mut self,
        block_height: BlockHeight,
    ) -> anyhow::Result<(Word, Bytes32)> {
        if !self.headers.contains_key(&block_height) {
            tracing::warn!(
                "Fetching header for block {} from the network.",
                block_height
            );

            let header = self.client.block_by_height(block_height).await?.ok_or(
                anyhow::anyhow!("Block header for block {} not found.", block_height),
            )?;

            let block_id = header.id;
            let timestamp = header.header.time.0;

            self.headers.insert(block_height, (timestamp, block_id));
        }

        Ok(self
            .headers
            .get(&block_height)
            .cloned()
            .expect("Header inserted above; qed"))
    }

    fn block_on<F, R>(future: F) -> R
    where
        F: Future<Output = R>,
    {
        tokio::task::block_in_place(|| Handle::current().block_on(future))
    }

    pub fn consensus_parameters(
        &mut self,
        version: ConsensusParametersVersion,
    ) -> anyhow::Result<Arc<ConsensusParameters>> {
        let result = self.consensus_parameters_inner(version).now_or_never();

        match result {
            None => Self::block_on(self.consensus_parameters_inner(version)),
            Some(result) => return result,
        }
    }

    async fn consensus_parameters_inner(
        &mut self,
        version: ConsensusParametersVersion,
    ) -> anyhow::Result<Arc<ConsensusParameters>> {
        if !self.consensus_parameters.contains_key(&version) {
            tracing::warn!(
                "Fetching consensus parameters for version {} from the network.",
                version
            );

            let consensus_parameters = self
                .client
                .consensus_parameters(version as i32)
                .await?
                .ok_or(anyhow::anyhow!(
                    "Not found consensus parameters {} version.",
                    version
                ))?;

            self.consensus_parameters
                .insert(version, Arc::new(consensus_parameters));
        }

        Ok(self
            .consensus_parameters
            .get(&version)
            .cloned()
            .expect("Consensus parameters inserted above; qed"))
    }

    pub fn blob(
        &mut self,
        blob_id: &BlobId,
    ) -> anyhow::Result<Option<&'static BlobBytes>> {
        let result = self.blob_inner(blob_id).now_or_never();

        match result {
            None => Self::block_on(self.blob_inner(blob_id)),
            Some(result) => return result,
        }
    }

    async fn blob_inner(
        &mut self,
        blob_id: &BlobId,
    ) -> anyhow::Result<Option<&'static BlobBytes>> {
        if !self.blobs.contains_key(blob_id) {
            tracing::warn!("Fetching blob {} from the network.", blob_id);

            let blob_bytes = self.client.blob(*blob_id).await?;

            self.blobs
                .insert(*blob_id, blob_bytes.map(|v| v.bytecode.into()));
        }

        Ok(self
            .blobs
            .get(blob_id)
            .expect("Blob inserted above; qed")
            .as_ref()
            .map(|v| unsafe { make_static(v) }))
    }

    pub fn slot(
        &mut self,
        key: &ContractsStateKey,
    ) -> anyhow::Result<Option<&'static ContractsStateData>> {
        let result = self.slot_inner(key).now_or_never();

        match result {
            None => Self::block_on(self.slot_inner(key)),
            Some(result) => return result,
        }
    }

    pub async fn slot_inner(
        &mut self,
        key: &ContractsStateKey,
    ) -> anyhow::Result<Option<&'static ContractsStateData>> {
        let block_height = self.block_height();
        let client = self.client.clone();
        let storage = self.contract_storage_inner(key.contract_id()).await?;

        let value = storage
            .slot(key.state_key(), &block_height, client.as_ref())
            .await?
            .map(|v| unsafe { make_static(v) });

        Ok(value)
    }

    pub fn asset(&mut self, key: &ContractsAssetKey) -> anyhow::Result<Option<Word>> {
        let result = self.asset_inner(key).now_or_never();

        match result {
            None => Self::block_on(self.asset_inner(key)),
            Some(result) => return result,
        }
    }

    pub async fn asset_inner(
        &mut self,
        key: &ContractsAssetKey,
    ) -> anyhow::Result<Option<Word>> {
        let block_height = self.block_height();
        let client = self.client.clone();
        let storage = self.contract_storage_inner(key.contract_id()).await?;

        let value = storage
            .asset(key.asset_id(), &block_height, client.as_ref())
            .await?;

        Ok(value)
    }

    pub(crate) async fn contract_storage_inner(
        &mut self,
        contract_id: &ContractId,
    ) -> anyhow::Result<&mut ContractStorage> {
        if !self.contracts_storage.contains_key(contract_id) {
            tracing::warn!(
                "Fetching contract storage for {} from the network.",
                contract_id
            );

            let storage = ContractStateLoader::new(*contract_id, self.client.clone())
                .load_contract_state(self.block_height())
                .await?;

            self.contracts_storage.insert(*contract_id, storage);
        }

        Ok(self
            .contracts_storage
            .get_mut(contract_id)
            .expect("Contract storage inserted above; qed"))
    }

    pub fn contract_bytecode(
        &mut self,
        contract_id: &ContractId,
    ) -> anyhow::Result<Option<&'static Contract>> {
        let result = self.contract_bytecode_inner(contract_id).now_or_never();

        match result {
            None => Self::block_on(self.contract_bytecode_inner(contract_id)),
            Some(result) => return result,
        }
    }

    async fn contract_bytecode_inner(
        &mut self,
        contract_id: &ContractId,
    ) -> anyhow::Result<Option<&'static Contract>> {
        if !self.contracts_bytecodes.contains_key(contract_id) {
            tracing::warn!(
                "Fetching contract bytecode for {} from the network.",
                contract_id
            );

            let bytecode = self.client.contract(contract_id).await?;

            self.contracts_bytecodes
                .insert(*contract_id, bytecode.map(|c| c.bytecode.into()));
        }

        Ok(self
            .contracts_bytecodes
            .get(contract_id)
            .expect("Contract bytecode inserted above; qed")
            .as_ref()
            .map(|v| unsafe { make_static(v) }))
    }

    pub fn apply_block(
        &mut self,
        block: Block,
        memory_instance: &mut MemoryInstance,
    ) -> anyhow::Result<()> {
        let params =
            &self.consensus_parameters(block.header().consensus_parameters_version())?;
        let block_height = *block.header().height();

        let executor_storage = ExecutionVMStorage::new(self, &block)?;
        let inter_params = InterpreterParams::new(0, params.deref());
        let mut interpreter = Interpreter::<_, _, _, NotSupportedEcal>::with_storage(
            memory_instance,
            executor_storage,
            inter_params,
        );

        let (_, transactions) = block.clone().into_inner();

        for tx in transactions {
            let Transaction::Script(script) = tx else {
                continue
            };

            let mut want_to_execute = false;

            for input in script.inputs() {
                if let Some(contract_id) = input.contract_id() {
                    if interpreter.as_ref().is_interesting_contract(&contract_id) {
                        want_to_execute = true;
                        break;
                    }
                }
            }

            if !want_to_execute {
                continue
            }

            let expected_receipts_root = *script.receipts_root();

            let tx = script
                .into_checked_basic(block_height, params)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let tx_id = tx.transaction().cached_id().unwrap();
            let tx = tx.test_into_ready();
            let state = interpreter
                .transact(tx)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let revert = state.should_revert();

            let actual_root = *state.tx().receipts_root();

            if expected_receipts_root != actual_root {
                return Err(anyhow::anyhow!(
                    "Receipts root mismatch: expected {}, got {} for {tx_id}. Receipts: {:?}",
                    expected_receipts_root,
                    actual_root,
                    state.receipts()
                ));
            }

            if !revert {
                interpreter.as_mut().commit_changes()?;
            } else {
                interpreter.as_mut().revert_changes();
            }
        }

        self.last_block = block;

        tracing::info!("Block applied: {}", **self.last_block.header().height());

        Ok(())
    }

    pub fn dry_run(
        &mut self,
        transactions: Vec<Transaction>,
        memory_instance: &mut MemoryInstance,
    ) -> anyhow::Result<Vec<Vec<Receipt>>> {
        let params = self.consensus_parameters(
            self.last_block.header().consensus_parameters_version(),
        )?;
        let block_height = *self.last_block.header().height();

        let executor_storage = ExecutionVMStorage::new(self, &self.last_block.clone())?;
        let inter_params = InterpreterParams::new(0, params.deref());
        let mut interpreter = Interpreter::<_, _, _, NotSupportedEcal>::with_storage(
            memory_instance,
            executor_storage,
            inter_params,
        );

        let mut receipts = Vec::with_capacity(transactions.len());

        for tx in transactions {
            let Transaction::Script(script) = tx else {
                continue
            };

            tracing::info!("Create snapshot");
            interpreter.as_mut().create_snapshot();

            let tx = script
                .into_checked_basic(block_height, params.deref())
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let tx = tx.test_into_ready();
            tracing::info!("Run transaction");

            let state = interpreter
                .transact(tx)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;

            receipts.push(state.receipts().to_vec());

            let revert = state.should_revert();

            if revert {
                interpreter.as_mut().restore_from_snapshot();
            }

            tracing::info!("Finished running snapshot");
        }

        Ok(receipts)
    }
}

unsafe fn make_static<T>(t: &T) -> &'static T {
    core::mem::transmute(t)
}
