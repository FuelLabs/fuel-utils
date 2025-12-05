use crate::vm_storage::VMStorage;
use anyhow::anyhow;
use fuel_core_types::blockchain::{
    block::Block,
    header::{
        ConsensusParametersVersion,
        StateTransitionBytecodeVersion,
    },
};
use fuel_storage::{
    StorageAsMut,
    StorageAsRef,
    StorageInspect,
    StorageMutate,
    StorageRead,
    StorageSize,
    StorageWrite,
};
use fuel_tx::{
    field::InputContract,
    ConsensusParameters,
    Contract,
    Transaction,
};
use fuel_types::{
    BlobId,
    BlockHeight,
    Bytes32,
    ContractId,
    Word,
};
use fuel_vm::{
    error::{
        InterpreterError,
        RuntimeError,
    },
    storage::{
        BlobBytes,
        BlobData,
        ContractsAssetKey,
        ContractsAssets,
        ContractsAssetsStorage,
        ContractsRawCode,
        ContractsState,
        ContractsStateData,
        ContractsStateKey,
        InterpreterStorage,
        UploadedBytecodes,
    },
};
use primitive_types::U256;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
};
use tokio::runtime::Handle;

pub struct ExecutionVMStorage<'a> {
    storage: RefCell<&'a mut VMStorage>,
    block_height: BlockHeight,
    consensus_parameters_version: ConsensusParametersVersion,
    state_transition_version: StateTransitionBytecodeVersion,
    timestamp: Word,
    coinbase: ContractId,
    slots_changes: HashMap<ContractsStateKey, Option<ContractsStateData>>,
    balances_changes: HashMap<ContractsAssetKey, Option<Word>>,
    snapshots: Vec<(
        HashMap<ContractsStateKey, Option<ContractsStateData>>,
        HashMap<ContractsAssetKey, Option<Word>>,
    )>,
}

impl<'s> ExecutionVMStorage<'s> {
    pub fn new(storage: &'s mut VMStorage, block: &Block) -> anyhow::Result<Self> {
        let Some(Transaction::Mint(mint)) = block.transactions().last() else {
            return Err(anyhow!("No mint transaction found in the block"));
        };

        let next_block_height = storage
            .block_height()
            .succ()
            .ok_or(anyhow!("Block height overflow"))?;

        if next_block_height != *block.header().height()
            && *block.header().height() != storage.block_height()
        {
            return Err(anyhow!("Block height mismatch"));
        }

        let coinbase = mint.input_contract().contract_id;
        let header = block.header();

        Ok(Self {
            storage: RefCell::new(storage),
            block_height: *header.height(),
            consensus_parameters_version: header.consensus_parameters_version(),
            state_transition_version: header.state_transition_bytecode_version(),
            timestamp: header.time().0,
            coinbase,
            slots_changes: HashMap::new(),
            balances_changes: HashMap::new(),
            snapshots: Default::default(),
        })
    }

    pub fn is_interesting_contract(&self, contract_id: &ContractId) -> bool {
        let storage = self.storage.borrow();
        storage.contracts_storage.contains_key(contract_id)
            || storage.contracts_bytecodes.contains_key(contract_id)
    }

    pub fn commit_changes(&mut self) -> anyhow::Result<()> {
        let mut storage = self.storage.borrow_mut();
        let slots = core::mem::take(&mut self.slots_changes);
        let balances = core::mem::take(&mut self.balances_changes);
        let result = tokio::task::block_in_place(|| {
            Handle::current().block_on(async move {
                for (key, value) in slots.into_iter() {
                    let storage =
                        storage.contract_storage_inner(key.contract_id()).await?;
                    storage.insert_slot(*key.state_key(), value);
                }

                for (key, value) in balances.into_iter() {
                    let storage =
                        storage.contract_storage_inner(key.contract_id()).await?;
                    storage.insert_asset(*key.asset_id(), value);
                }

                Ok(())
            })
        });
        self.snapshots.clear();
        result
    }

    pub fn revert_changes(&mut self) {
        self.slots_changes.clear();
        self.balances_changes.clear();
    }

    pub fn create_snapshot(&mut self) {
        self.snapshots
            .push((self.slots_changes.clone(), self.balances_changes.clone()));
    }

    pub fn restore_from_snapshot(&mut self) {
        let (slots, balances) = self.snapshots.pop().expect("No snapshot to restore");
        self.slots_changes = slots;
        self.balances_changes = balances;
    }
}

impl<'s> InterpreterStorage for ExecutionVMStorage<'s> {
    type DataError = StorageError;

    fn block_height(&self) -> Result<BlockHeight, Self::DataError> {
        Ok(self.block_height)
    }

    fn consensus_parameters_version(&self) -> Result<u32, Self::DataError> {
        Ok(self.consensus_parameters_version)
    }

    fn state_transition_version(&self) -> Result<u32, Self::DataError> {
        Ok(self.state_transition_version)
    }

    fn timestamp(&self, height: BlockHeight) -> Result<Word, Self::DataError> {
        let timestamp = match height {
            // panic if $rB is greater than the current block height.
            height if height > self.block_height => {
                return Err(anyhow!("block height too high for timestamp").into())
            }
            height if height == self.block_height => self.timestamp,
            height => self.storage.borrow_mut().header(height)?.0,
        };
        Ok(timestamp)
    }

    fn block_hash(&self, block_height: BlockHeight) -> Result<Bytes32, Self::DataError> {
        if block_height >= self.block_height || block_height == Default::default() {
            Ok(Bytes32::zeroed())
        } else {
            Ok(self.storage.borrow_mut().header(block_height)?.1)
        }
    }

    fn coinbase(&self) -> Result<ContractId, Self::DataError> {
        Ok(self.coinbase)
    }

    fn set_consensus_parameters(
        &mut self,
        _: u32,
        _: &ConsensusParameters,
    ) -> Result<Option<ConsensusParameters>, Self::DataError> {
        unreachable!("It is executor only for the `Script`")
    }

    fn set_state_transition_bytecode(
        &mut self,
        _: u32,
        _: &Bytes32,
    ) -> Result<Option<Bytes32>, Self::DataError> {
        unreachable!("It is executor only for the `Script`")
    }

    fn contract_state_range(
        &self,
        contract_id: &ContractId,
        start_key: &Bytes32,
        range: usize,
    ) -> Result<Vec<Option<Cow<'_, ContractsStateData>>>, Self::DataError> {
        let mut key = U256::from_big_endian(start_key.as_ref());
        let mut state_key = Bytes32::zeroed();

        let mut results = Vec::new();
        for i in 0..range {
            if i != 0 {
                key.increase()?;
            }
            key.to_big_endian(state_key.as_mut());
            let multikey = ContractsStateKey::new(contract_id, &state_key);
            results.push(self.storage_as_ref::<ContractsState>().get(&multikey)?);
        }
        Ok(results)
    }

    fn contract_state_insert_range<'a, I>(
        &mut self,
        contract_id: &ContractId,
        start_key: &Bytes32,
        values: I,
    ) -> Result<usize, Self::DataError>
    where
        I: Iterator<Item = &'a [u8]>,
    {
        let values: Vec<_> = values.collect();
        let mut current_key = U256::from_big_endian(start_key.as_ref());

        // verify key is in range
        current_key
            .checked_add(U256::from(values.len().saturating_sub(1)))
            .ok_or_else(|| anyhow!("range op exceeded available keyspace"))?;

        let mut key_bytes = Bytes32::zeroed();
        let mut found_unset = 0u32;
        for (idx, value) in values.iter().enumerate() {
            if idx != 0 {
                current_key.increase()?;
            }
            current_key.to_big_endian(key_bytes.as_mut());

            let option = self
                .storage_as_mut::<ContractsState>()
                .replace(&(contract_id, &key_bytes).into(), value)?;

            if option.is_none() {
                found_unset = found_unset
                    .checked_add(1)
                    .expect("We've checked it above via `values.len()`");
            }
        }

        Ok(found_unset as usize)
    }

    fn contract_state_remove_range(
        &mut self,
        contract_id: &ContractId,
        start_key: &Bytes32,
        range: usize,
    ) -> Result<Option<()>, Self::DataError> {
        let mut found_unset = false;

        let mut current_key = U256::from_big_endian(start_key.as_ref());

        let mut key_bytes = Bytes32::zeroed();
        for i in 0..range {
            if i != 0 {
                current_key.increase()?;
            }
            current_key.to_big_endian(key_bytes.as_mut());

            let option = self
                .storage_as_mut::<ContractsState>()
                .take(&(contract_id, &key_bytes).into())?;

            found_unset |= option.is_none();
        }

        if found_unset {
            Ok(None)
        } else {
            Ok(Some(()))
        }
    }
}

mod contract_bytecode {
    use super::*;
    use std::ops::Deref;

    impl<'s> StorageInspect<ContractsRawCode> for ExecutionVMStorage<'s> {
        type Error = StorageError;

        fn get(
            &self,
            key: &ContractId,
        ) -> Result<Option<Cow<'_, Contract>>, Self::Error> {
            self.storage
                .borrow_mut()
                .contract_bytecode(key)
                .map(|v| v.map(Cow::Borrowed))
                .map_err(Into::into)
        }

        fn contains_key(&self, key: &ContractId) -> Result<bool, Self::Error> {
            <_ as StorageInspect<ContractsRawCode>>::get(self, key).map(|v| v.is_some())
        }
    }

    impl<'s> StorageMutate<ContractsRawCode> for ExecutionVMStorage<'s> {
        fn replace(
            &mut self,
            _: &ContractId,
            _: &[u8],
        ) -> Result<Option<Contract>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn take(&mut self, _: &ContractId) -> Result<Option<Contract>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }
    }

    impl<'s> StorageWrite<ContractsRawCode> for ExecutionVMStorage<'s> {
        fn write_bytes(&mut self, _: &ContractId, _: &[u8]) -> Result<(), Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn replace_bytes(
            &mut self,
            _: &ContractId,
            _: &[u8],
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn take_bytes(&mut self, _: &ContractId) -> Result<Option<Vec<u8>>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }
    }

    impl<'s> StorageSize<ContractsRawCode> for ExecutionVMStorage<'s> {
        fn size_of_value(&self, key: &ContractId) -> Result<Option<usize>, Self::Error> {
            <_ as StorageInspect<ContractsRawCode>>::get(self, key)
                .map(|v| v.map(|v| v.len()))
        }
    }

    impl<'s> StorageRead<ContractsRawCode> for ExecutionVMStorage<'s> {
        fn read(
            &self,
            key: &ContractId,
            offset: usize,
            buf: &mut [u8],
        ) -> Result<bool, Self::Error> {
            let value = <_ as StorageInspect<ContractsRawCode>>::get(self, key)?;

            let result = value
                .map(|c| {
                    let contract_len = c.as_ref().len();
                    let start = offset;
                    let end = offset.saturating_add(buf.len());
                    if end > contract_len {
                        return Err::<(), _>(anyhow::anyhow!(
                            "Contract offset out of bounds"
                        ));
                    }

                    let starting_from_offset = &c.deref().as_ref()[start..end];
                    buf[..].copy_from_slice(starting_from_offset);
                    Ok(())
                })
                .transpose()?;

            Ok(result.is_some())
        }

        fn read_alloc(&self, key: &ContractId) -> Result<Option<Vec<u8>>, Self::Error> {
            let value = <_ as StorageInspect<ContractsRawCode>>::get(self, key)?;
            Ok(value.map(|v| v.into_owned().into()))
        }
    }
}

mod contract_state {
    use super::*;

    impl<'s> StorageInspect<ContractsState> for ExecutionVMStorage<'s> {
        type Error = StorageError;

        fn get(
            &self,
            key: &ContractsStateKey,
        ) -> Result<Option<Cow<'_, ContractsStateData>>, Self::Error> {
            if let Some(value) = self.slots_changes.get(&key) {
                return Ok(value.as_ref().map(|v| Cow::Borrowed(v)));
            }

            let value = self.storage.borrow_mut().slot(key)?;

            Ok(value.map(|v| Cow::Owned(v)))
        }

        fn contains_key(&self, key: &ContractsStateKey) -> Result<bool, Self::Error> {
            <_ as StorageInspect<ContractsState>>::get(self, key).map(|v| v.is_some())
        }
    }

    impl<'s> StorageMutate<ContractsState> for ExecutionVMStorage<'s> {
        fn replace(
            &mut self,
            key: &ContractsStateKey,
            value: &[u8],
        ) -> Result<Option<ContractsStateData>, Self::Error> {
            let old = <_ as StorageInspect<ContractsState>>::get(self, key)?
                .map(|v| v.into_owned());

            self.slots_changes.insert(*key, Some(value.to_vec().into()));

            Ok(old)
        }

        fn take(
            &mut self,
            key: &ContractsStateKey,
        ) -> Result<Option<ContractsStateData>, Self::Error> {
            let old = <_ as StorageInspect<ContractsState>>::get(self, key)?
                .map(|v| v.into_owned());

            self.slots_changes.insert(*key, None);

            Ok(old)
        }
    }

    impl<'s> StorageWrite<ContractsState> for ExecutionVMStorage<'s> {
        fn write_bytes(
            &mut self,
            key: &ContractsStateKey,
            buf: &[u8],
        ) -> Result<(), Self::Error> {
            self.slots_changes
                .insert(*key, Some(ContractsStateData::from(buf)));
            Ok(())
        }

        fn replace_bytes(
            &mut self,
            key: &ContractsStateKey,
            buf: &[u8],
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            let old = <_ as StorageInspect<ContractsState>>::get(self, key)?
                .map(|v| v.into_owned());
            self.slots_changes
                .insert(*key, Some(ContractsStateData::from(buf)));

            Ok(old.map(|v| v.into()))
        }

        fn take_bytes(
            &mut self,
            key: &ContractsStateKey,
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            let old = <_ as StorageInspect<ContractsState>>::get(self, key)?
                .map(|v| v.into_owned());
            self.slots_changes.insert(*key, None);

            Ok(old.map(|v| v.into()))
        }
    }
    impl<'s> StorageSize<ContractsState> for ExecutionVMStorage<'s> {
        fn size_of_value(
            &self,
            key: &ContractsStateKey,
        ) -> Result<Option<usize>, Self::Error> {
            Ok(<_ as StorageInspect<ContractsState>>::get(self, key)?.map(|v| v.0.len()))
        }
    }
    impl<'s> StorageRead<ContractsState> for ExecutionVMStorage<'s> {
        fn read(
            &self,
            key: &ContractsStateKey,
            offset: usize,
            buf: &mut [u8],
        ) -> Result<bool, Self::Error> {
            let value = <_ as StorageInspect<ContractsState>>::get(self, key)?;

            let result = value
                .map(|v| {
                    let value_len = v.0.len();
                    let start = offset;
                    let end = offset.saturating_add(buf.len());
                    if end > value_len {
                        return Err::<(), Self::Error>(
                            anyhow::anyhow!("Value offset out of bounds").into(),
                        );
                    }

                    let starting_from_offset = &v.0[start..end];
                    buf[..].copy_from_slice(starting_from_offset);
                    Ok(())
                })
                .transpose()?;

            Ok(result.is_some())
        }

        fn read_alloc(
            &self,
            key: &ContractsStateKey,
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            let value = <_ as StorageInspect<ContractsState>>::get(self, key)?;
            Ok(value.map(|v| v.into_owned().into()))
        }
    }
}

mod upload {
    use super::*;
    use fuel_vm::storage::UploadedBytecode;

    impl<'s> StorageInspect<UploadedBytecodes> for ExecutionVMStorage<'s> {
        type Error = StorageError;

        fn get(
            &self,
            _: &Bytes32,
        ) -> Result<Option<Cow<'_, UploadedBytecode>>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn contains_key(&self, _: &Bytes32) -> Result<bool, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }
    }

    impl<'s> StorageMutate<UploadedBytecodes> for ExecutionVMStorage<'s> {
        fn replace(
            &mut self,
            _: &Bytes32,
            _: &UploadedBytecode,
        ) -> Result<Option<UploadedBytecode>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn take(&mut self, _: &Bytes32) -> Result<Option<UploadedBytecode>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }
    }
}

mod blob {
    use super::*;
    impl<'s> StorageInspect<BlobData> for ExecutionVMStorage<'s> {
        type Error = StorageError;

        fn get(&self, key: &BlobId) -> Result<Option<Cow<'_, BlobBytes>>, Self::Error> {
            Ok(self
                .storage
                .borrow_mut()
                .blob(key)?
                .map(|v| Cow::Borrowed(v)))
        }

        fn contains_key(&self, key: &BlobId) -> Result<bool, Self::Error> {
            <_ as StorageInspect<BlobData>>::get(self, key).map(|v| v.is_some())
        }
    }

    impl<'s> StorageMutate<BlobData> for ExecutionVMStorage<'s> {
        fn replace(
            &mut self,
            _: &BlobId,
            _: &[u8],
        ) -> Result<Option<BlobBytes>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn take(&mut self, _: &BlobId) -> Result<Option<BlobBytes>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }
    }

    impl<'s> StorageWrite<BlobData> for ExecutionVMStorage<'s> {
        fn write_bytes(&mut self, _: &BlobId, _: &[u8]) -> Result<(), Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn replace_bytes(
            &mut self,
            _: &BlobId,
            _: &[u8],
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }

        fn take_bytes(&mut self, _: &BlobId) -> Result<Option<Vec<u8>>, Self::Error> {
            unreachable!("Executor only process Script transactions")
        }
    }

    impl<'s> StorageSize<BlobData> for ExecutionVMStorage<'s> {
        fn size_of_value(&self, key: &BlobId) -> Result<Option<usize>, Self::Error> {
            Ok(<_ as StorageInspect<BlobData>>::get(self, key)?.map(|v| v.0.len()))
        }
    }

    impl<'s> StorageRead<BlobData> for ExecutionVMStorage<'s> {
        fn read(
            &self,
            key: &BlobId,
            offset: usize,
            buf: &mut [u8],
        ) -> Result<bool, Self::Error> {
            let value = <_ as StorageInspect<BlobData>>::get(self, key)?;

            let result = value
                .map(|v| {
                    let blob_len = v.0.len();
                    let start = offset;
                    let end = offset.saturating_add(buf.len());
                    if end > blob_len {
                        return Err::<(), Self::Error>(
                            anyhow::anyhow!("blob offset out of bounds").into(),
                        );
                    }

                    let starting_from_offset = &v.0[start..end];
                    buf[..].copy_from_slice(starting_from_offset);
                    Ok(())
                })
                .transpose()?;

            Ok(result.is_some())
        }

        fn read_alloc(&self, key: &BlobId) -> Result<Option<Vec<u8>>, Self::Error> {
            let value = <_ as StorageInspect<BlobData>>::get(self, key)?;
            Ok(value.map(|v| v.into_owned().into()))
        }
    }
}

mod contract_balances {
    use super::*;
    use fuel_vm::storage::ContractsAssetKey;

    impl<'s> StorageInspect<ContractsAssets> for ExecutionVMStorage<'s> {
        type Error = StorageError;

        fn get(
            &self,
            key: &ContractsAssetKey,
        ) -> Result<Option<Cow<'_, Word>>, Self::Error> {
            if let Some(value) = self.balances_changes.get(key) {
                return Ok(value.as_ref().map(|v| Cow::Borrowed(v)));
            }

            let value = self.storage.borrow_mut().asset(key)?;

            Ok(value.map(|v| Cow::Owned(v)))
        }

        fn contains_key(&self, key: &ContractsAssetKey) -> Result<bool, Self::Error> {
            <_ as StorageInspect<ContractsAssets>>::get(self, key).map(|v| v.is_some())
        }
    }

    impl<'s> StorageMutate<ContractsAssets> for ExecutionVMStorage<'s> {
        fn replace(
            &mut self,
            key: &ContractsAssetKey,
            value: &Word,
        ) -> Result<Option<Word>, Self::Error> {
            let old = <_ as StorageInspect<ContractsAssets>>::get(self, key)?
                .map(|v| v.into_owned());

            self.balances_changes.insert(*key, Some(*value));

            Ok(old)
        }

        fn take(&mut self, key: &ContractsAssetKey) -> Result<Option<Word>, Self::Error> {
            let old = <_ as StorageInspect<ContractsAssets>>::get(self, key)?
                .map(|v| v.into_owned());

            self.balances_changes.insert(*key, None);

            Ok(old)
        }
    }

    impl<'s> ContractsAssetsStorage for ExecutionVMStorage<'s> {}
}

/// The trait around the `U256` type allows increasing the key by one.
pub trait IncreaseStorageKey {
    /// Increases the key by one.
    ///
    /// Returns a `Result::Err` in the case of overflow.
    fn increase(&mut self) -> anyhow::Result<()>;
}

impl IncreaseStorageKey for U256 {
    fn increase(&mut self) -> anyhow::Result<()> {
        *self = self
            .checked_add(1.into())
            .ok_or_else(|| anyhow!("range op exceeded available keyspace"))?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum StorageError {
    Other(anyhow::Error),
}

impl From<anyhow::Error> for StorageError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl From<StorageError> for RuntimeError<StorageError> {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}

impl From<StorageError> for InterpreterError<StorageError> {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}
