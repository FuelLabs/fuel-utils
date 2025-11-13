use crate::{
    full_block::ClientExt,
    vm_storage::VMStorage,
};
use fuel_core_client::client::FuelClient;
use fuel_core_types::blockchain::block::Block;
use fuel_tx::{
    Receipt,
    Transaction,
};
use fuel_types::{
    BlobId,
    BlockHeight,
    ContractId,
};
use fuel_vm::prelude::MemoryInstance;
use reqwest::Url;
use std::{
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;

pub enum WhatToOverride {
    Contract { contract_id: ContractId },
    Blob { blob_id: BlobId },
}

pub enum Event {
    Override {
        what: WhatToOverride,
        value: Option<Vec<u8>>,
    },
    Snapshot {
        sender: tokio::sync::oneshot::Sender<()>,
    },
    DryRun {
        txs: Vec<Transaction>,
        commit_changes: bool,
        response: tokio::sync::oneshot::Sender<anyhow::Result<Vec<Vec<Receipt>>>>,
    },
}

pub struct ExecutorInner {
    vm_storage: VMStorage,
    path: PathBuf,
    dry_run_sender: mpsc::Sender<Event>,
    dry_run_queue: mpsc::Receiver<Event>,
    blocks_sender: mpsc::Sender<Block>,
    blocks: mpsc::Receiver<Block>,
}

impl ExecutorInner {
    pub async fn new(
        block_height: Option<BlockHeight>,
        path: PathBuf,
        url: Url,
    ) -> anyhow::Result<Self> {
        let (events_sender, events_queue) = mpsc::channel(1024);
        let (blocks_sender, blocks) = mpsc::channel(1024);
        let client = FuelClient::new(url.clone())?;
        let client = Arc::new(client);

        let vm_storage = match VMStorage::from_storage(client.clone(), path.clone()) {
            Ok(vm_storage) => vm_storage,
            Err(_) => {
                tracing::warn!(
                    "Failed to load VMStorage from storage. Initiating a new one",
                );

                let latest_height = if let Some(block_height) = block_height {
                    *block_height
                } else {
                    client.chain_info().await?.latest_block.header.height
                };
                let blocks = client.full_blocks(latest_height..latest_height + 1).await?;
                let block = blocks.into_iter().next().unwrap();

                VMStorage::new(client, block)
            }
        };

        Ok(Self {
            vm_storage,
            path,
            dry_run_sender: events_sender,
            dry_run_queue: events_queue,
            blocks_sender,
            blocks,
        })
    }

    pub fn block_height(&self) -> BlockHeight {
        self.vm_storage.block_height()
    }

    pub fn dry_run_sender(&self) -> mpsc::Sender<Event> {
        self.dry_run_sender.clone()
    }

    pub fn blocks_sender(&self) -> mpsc::Sender<Block> {
        self.blocks_sender.clone()
    }

    pub fn dry_run(
        &mut self,
        transactions: Vec<Transaction>,
        memory: &mut MemoryInstance,
        commit_changes: bool,
    ) -> anyhow::Result<Vec<Vec<Receipt>>> {
        self.vm_storage
            .dry_run(transactions, memory, commit_changes)
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut memory = MemoryInstance::new();

        loop {
            tokio::select! {
                biased;
                _ = tokio::signal::ctrl_c() => {
                    break;
                }

                command = self.dry_run_queue.recv() => {
                    match command {
                        None => {
                            tracing::info!("Dry run channel closed, shutting down executor");
                            break;
                        }
                        Some(command) => {
                            match command {
                                Event::DryRun{ txs, commit_changes, response } => {
                                    tracing::info!("Dry running transactions");
                                    let receipts = self
                                        .dry_run(txs, &mut memory, commit_changes);
                                    tracing::info!("Finished dry running transactions");
                                    if let Err(_) = response.send(receipts) {
                                        tracing::error!("Failed to send receipts");
                                        break
                                    }
                                }
                                Event::Override{ what, value } => {
                                    match what {
                                        WhatToOverride::Contract { contract_id } => {
                                            self.vm_storage.contracts_bytecodes.insert(contract_id, value.map(Into::into));
                                        }
                                        WhatToOverride::Blob { blob_id } => {
                                            self.vm_storage.blobs.insert(blob_id, value.map(Into::into));
                                        }
                                    }
                                }
                                Event::Snapshot { sender }  => {
                                    let result = self.vm_storage.to_storage(&self.path);
                                    if let Err(e) = result {
                                        tracing::error!("Failed to save VMStorage to storage: {:?}", e);
                                    }
                                    let _ = sender.send(());
                                }
                            }
                        }
                    }
                }
                block = self.blocks.recv() => {
                    if let Some(block) = block {
                        self.vm_storage.apply_block(block, &mut memory)?;
                    } else {
                        break
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone)]
pub struct Executor {
    sender: mpsc::Sender<Event>,
}

impl Executor {
    pub async fn new(
        block_height: Option<BlockHeight>,
        path: PathBuf,
        url: Url,
        follow_blocks: bool,
    ) -> anyhow::Result<Self> {
        let executor_inner = ExecutorInner::new(block_height, path, url.clone()).await?;

        let block_sender = executor_inner.blocks_sender();
        let starting_block_height = executor_inner.block_height();

        tokio::spawn(async move {
            let client = FuelClient::new(url).unwrap();
            if !follow_blocks {
                tokio::time::sleep(Duration::from_secs(60 * 60 * 24 * 365)).await;
                drop(block_sender);
                return
            }

            let mut starting_block_height = *starting_block_height + 1;
            loop {
                let last_block_height = starting_block_height + 100;
                let range = starting_block_height..last_block_height;

                let Ok(blocks) = client.full_blocks(range.clone()).await else {
                    tracing::error!("Failed to fetch blocks for range: {:?}", range);
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue
                };

                for block in blocks {
                    starting_block_height = **block.header().height() + 1;
                    block_sender.send(block).await.unwrap();
                }
                tokio::time::sleep(Duration::from_millis(400)).await
            }
        });

        let sender = executor_inner.dry_run_sender();
        std::thread::spawn(move || {
            // Create a new Runtime to run tasks
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(1)
                .on_thread_start(move || {
                    thread_priority::set_current_thread_priority(
                        thread_priority::ThreadPriority::Max,
                    )
                    .expect("Setting thread priority")
                })
                .build()
                .expect("Creating Tokio runtime");

            runtime.block_on(async move {
                let result = executor_inner.run().await;
                if let Err(e) = result {
                    tracing::error!("Executor failed: {:?}", e);
                }
            });
        });

        Ok(Self { sender })
    }

    pub async fn snapshot(&self) -> anyhow::Result<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.sender.send(Event::Snapshot { sender }).await?;
        receiver.await?;
        Ok(())
    }

    pub async fn override_state(
        &self,
        what: WhatToOverride,
        value: Option<Vec<u8>>,
    ) -> anyhow::Result<()> {
        self.sender.send(Event::Override { what, value }).await?;
        Ok(())
    }

    pub async fn dry_run(
        &self,
        transactions: Vec<Transaction>,
        commit_changes: bool,
    ) -> anyhow::Result<Vec<Vec<Receipt>>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.sender
            .send(Event::DryRun {
                txs: transactions,
                commit_changes,
                response: sender,
            })
            .await?;
        receiver.await?
    }
}

#[cfg(test)]
mod tests {
    use crate::executor::Executor;
    use fuel_types::canonical::Deserialize;
    use std::path::PathBuf;

    const URL: &str = "http://localhost:4001";

    #[tokio::test(flavor = "multi_thread")]
    async fn can_dry_run_locally() {
        let tx = "00000000000000000000000000249f00ee34f389f380e04c0339668ab684e3a9415891f165aecfb862757345f8dd2fc600000000000002f000000000000000a500000000000000080000000000000004000000000000000500000000000000011a403000504100301a445000ba49000032400481504100205d490000504100083240048220451300524510044a4400000ace23070489e6ae77d4938e3ad43f763a1e6650286e77429bb3785c24a7dec800000000000002982e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e700000000ffffffff446561646c696e652070617373656400526f757465723a20494e56414c49445f504154480000000066656573000000000000000000000000000000000000000000000000000000000000000000000000706f6f6c5f6d65746164617461000000506f6f6c206e6f742070726573656e74496e73756666696369656e74206f757470757420616d6f756e740000000000007377617000000000ffffffffffffffffcccccccccccc0002ffffffffffff00018c25cb3686462e9affffffffffff000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000de0b6b3a7640000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000027100000000000003d940000000000003d9c0000000000003dac0000000000003ac400000000000039c000000000000030600000000000002aa00000000000002a80000000000000276400000000000025dc00000000000011b8000000000000119c000000000000116c0000000000001148000000000000112400000000000010f4000000000000105400000000000010280000000000000ff40000000000000fd00000000000000dcc0000000000000d4c0000000000000cbc0000000000000c800000000000000c480000000000000c280000000000000c0c0000000000000bec0000000000000ae00000000000000970000000000000089400000000000007d40000000000000738000000000000068c00000000000006f400000000000006d000000000000005cc000000000000063c00000000039dfeb41493d4ec82124de8f9b625682de69dcccda79e882b89a55a8c737b12de67bd6800000000042331db00000000000000011493d4ec82124de8f9b625682de69dcccda79e882b89a55a8c737b12de67bd68f8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07010000000000000000d19de8901d187b74b4aa081b63e13443cffe9cd89fc5fd58236fea25742d9419ffffffff000000000000000000e1860000000000000001b9f9f7a9d29f55a0557b1cce1e40195ecad86c4914364776f68eba4c18fa21a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000cc129e00000000000000002e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e700000000000000001226374f1ff1ad5e1e3a124449dbc1f801c7cecebdd1528790331f282d0209fd0000000000000001d19de8901d187b74b4aa081b63e13443cffe9cd89fc5fd58236fea25742d941900000000039dfeb41493d4ec82124de8f9b625682de69dcccda79e882b89a55a8c737b12de67bd680000000000cc127f0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001226374f1ff1ad5e1e3a124449dbc1f801c7cecebdd1528790331f282d0209fd0000000000000004d19de8901d187b74b4aa081b63e13443cffe9cd89fc5fd58236fea25742d9419000000000022c8edf8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad070000000000cc127f000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001b9f9f7a9d29f55a0557b1cce1e40195ecad86c4914364776f68eba4c18fa21a00000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000cc129e0000000000000000a703db08d1dbf30a6cd2fef942d8dcf03f25d2254e2091ee1f97bf5fa615639e00000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003d19de8901d187b74b4aa081b63e13443cffe9cd89fc5fd58236fea25742d941900000000042de4c9f8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad070000000000000002d19de8901d187b74b4aa081b63e13443cffe9cd89fc5fd58236fea25742d941900000000000000001493d4ec82124de8f9b625682de69dcccda79e882b89a55a8c737b12de67bd680000000000000002d19de8901d187b74b4aa081b63e13443cffe9cd89fc5fd58236fea25742d9419000000000022ba00f8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07000000000000000100000000000000030000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000400bfb0dc20d86eb35617a45b23b4b76a7c94ad5369c7ce903cf491aa2d111b99d3e6e885f60f07e0462f56001c9213e0a545649b82ce8802ca2a51f0fd89ccf7e";
        let bytes = hex::decode(tx).unwrap();

        let tx = fuel_tx::Transaction::from_bytes(&bytes).unwrap();
        let executor = Executor::new(
            Some(13374110u32.into()),
            PathBuf::new(),
            URL.parse().unwrap(),
            false,
        )
        .await
        .unwrap();

        let receipts = executor.dry_run(vec![tx.clone()]).await;
        tracing::info!("Receipts: {:?}", receipts);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn issue_with_executor() {
        let first_tx = "0000000000000000000000000013d6202b6222799c4732c69ebc86f70dd84a5de747686d27f83e9d2fa9eb2e9785f4e3000000000000001800000000000001e8000000000000000b000000000000000400000000000000050000000000000001724028c0724428985d451000724828a02d41148a2404000000000000001b9c7cf8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07e13e7a4ac692200044749b64e9faccf3587769254966a8519edb407a9b18d32c00000000000028f000000000000028fc0000000000000004737761702e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e700000000001b9c7cf8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad0700000000001b9c7c00000000000000041d5d97005e41cae2187a895fd8eab0506111e0e2f3331cd3912c15c24e3c1d82f8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07001493d4ec82124de8f9b625682de69dcccda79e882b89a55a8c737b12de67bd681d5d97005e41cae2187a895fd8eab0506111e0e2f3331cd3912c15c24e3c1d82001493d4ec82124de8f9b625682de69dcccda79e882b89a55a8c737b12de67bd68286c479da40dc953bddc3bb4c453b608bba2e0ac483b077bd475174115395e6b00286c479da40dc953bddc3bb4c453b608bba2e0ac483b077bd475174115395e6bf8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07000000000000000000e8b0bfdd9f748e28b8e8a7f682c034232ea2d18fbcbd2bd0e65a6943de9d68c90000000000000000000000000000004800000000000186a000000000000000017a5a82cc3071a6f2d68be32e0098cf2daee6d4761cf409c6bbc33cbeebbfae590000000000000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c6b72e0000000000000000a703db08d1dbf30a6cd2fef942d8dcf03f25d2254e2091ee1f97bf5fa615639e00000000000000015ac34ec75f356ed7b5d6d6111fb45801d91f1925fa275bc2d3227a47e5c708c20000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c6b5ea0000000000000002e13e7a4ac692200044749b64e9faccf3587769254966a8519edb407a9b18d32c00000000000000017a5a82cc3071a6f2d68be32e0098cf2daee6d4761cf409c6bbc33cbeebbfae590000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c6b72e00000000000000002e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e700000000000000005ac34ec75f356ed7b5d6d6111fb45801d91f1925fa275bc2d3227a47e5c708c20000000000000003e8b0bfdd9f748e28b8e8a7f682c034232ea2d18fbcbd2bd0e65a6943de9d68c900000000038c9c8bf8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad070000000000c6b5ea000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002e8b0bfdd9f748e28b8e8a7f682c034232ea2d18fbcbd2bd0e65a6943de9d68c9000000000370d86ef8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad070000000000000003e8b0bfdd9f748e28b8e8a7f682c034232ea2d18fbcbd2bd0e65a6943de9d68c900000000001bae59f8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07000000000000004047fd72d6e465594db685580d3e108d222ba2a79f84ef9c2e7f597115d909b074e0bc1b60a5f62919fd73ab32d9b6ef700848106d0aef0d4f5bac1c906f8e1f47";
        let first_tx_bytes = hex::decode(first_tx).unwrap();

        let first_tx = fuel_tx::Transaction::from_bytes(&first_tx_bytes).unwrap();
        let second_tx = "00000000000000000000000000249f00be32c7a9ea674d1f2c2b3d2217022517603b041cfa6536150bd22d23b6c7f85c00000000000002f000000000000000a500000000000000080000000000000004000000000000000500000000000000011a403000504100301a445000ba49000032400481504100205d490000504100083240048220451300524510044a4400000ace23070489e6ae77d4938e3ad43f763a1e6650286e77429bb3785c24a7dec800000000000002982e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e700000000ffffffff446561646c696e652070617373656400526f757465723a20494e56414c49445f504154480000000066656573000000000000000000000000000000000000000000000000000000000000000000000000706f6f6c5f6d65746164617461000000506f6f6c206e6f742070726573656e74496e73756666696369656e74206f757470757420616d6f756e740000000000007377617000000000ffffffffffffffffcccccccccccc0002ffffffffffff00018c25cb3686462e9affffffffffff000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000de0b6b3a7640000000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000027100000000000003d940000000000003d9c0000000000003dac0000000000003ac400000000000039c000000000000030600000000000002aa00000000000002a80000000000000276400000000000025dc00000000000011b8000000000000119c000000000000116c0000000000001148000000000000112400000000000010f4000000000000105400000000000010280000000000000ff40000000000000fd00000000000000dcc0000000000000d4c0000000000000cbc0000000000000c800000000000000c480000000000000c280000000000000c0c0000000000000bec0000000000000ae00000000000000970000000000000089400000000000007d40000000000000738000000000000068c00000000000006f400000000000006d000000000000005cc000000000000063c00001642a125ed701d5d97005e41cae2187a895fd8eab0506111e0e2f3331cd3912c15c24e3c1d82000000000ba76d2300000000000000011d5d97005e41cae2187a895fd8eab0506111e0e2f3331cd3912c15c24e3c1d82f8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07000000000000000000c452231484240e7117f95a082295004b09dd3086b159406def7bacb7acb5af2bffffffff00000000000000000056f3000000000000000151fa794a490dadf2422912c1ed7cc3456684f3f24398c97d7116ed7cb30a1a440000000000000002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c6b76200000000000000002e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e700000000000000008f8190feab57ee79e512efbc66757bb766cc8cf12415c3c64094b6655c8fd82c0000000000000031c452231484240e7117f95a082295004b09dd3086b159406def7bacb7acb5af2b00001642a125ed701d5d97005e41cae2187a895fd8eab0506111e0e2f3331cd3912c15c24e3c1d820000000000c585ef00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000092c9d54a282678849769a0f5f9ce01d66576dc69e9ecd5c440d9ddfbcf5eb70f0000000000000002c452231484240e7117f95a082295004b09dd3086b159406def7bacb7acb5af2b00000000000ace7ff8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad070000000000b6f4d800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000151fa794a490dadf2422912c1ed7cc3456684f3f24398c97d7116ed7cb30a1a440000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c6b7620000000000000000a703db08d1dbf30a6cd2fef942d8dcf03f25d2254e2091ee1f97bf5fa615639e00000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003c452231484240e7117f95a082295004b09dd3086b159406def7bacb7acb5af2b000000000bc5a72af8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad070000000000000002c452231484240e7117f95a082295004b09dd3086b159406def7bacb7acb5af2b00000000000000001d5d97005e41cae2187a895fd8eab0506111e0e2f3331cd3912c15c24e3c1d820000000000000002c452231484240e7117f95a082295004b09dd3086b159406def7bacb7acb5af2b00000000000abffbf8f8b6283d7fa5b672b530cbb84fcccb4ff8dc40f8176ef4544ddb1f1952ad07000000000000000100000000000000030000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000403c614d249188e7cea89a5f592174f523363c4f5f235aa6c755c03cd6e0425f5823cd2e5dc624228eff4ba4c4c5268e2b1411b20e0efc1c9c4bdaf043c5342995";
        let second_tx_bytes = hex::decode(second_tx).unwrap();

        let second_tx = fuel_tx::Transaction::from_bytes(&second_tx_bytes).unwrap();
        let executor = Executor::new(
            Some(13023073u32.into()),
            PathBuf::new(),
            URL.parse().unwrap(),
            false,
        )
        .await
        .unwrap();

        let receipts = executor.dry_run(vec![first_tx, second_tx]).await;
        println!("Receipts: {:?}", receipts);
    }
}
