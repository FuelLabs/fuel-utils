use crate::contract_storage::ContractStorage;
use fuel_core_client::client::FuelClient;
use fuel_types::{
    AssetId,
    BlockHeight,
    ContractId,
};
use futures::{
    StreamExt,
    TryStreamExt,
};
use itertools::Itertools;
use std::sync::Arc;

pub struct ContractStateLoader {
    contract_id: ContractId,
    client: Arc<FuelClient>,
}

const CHUNK_SIZE: usize = 1000;

impl ContractStateLoader {
    pub fn new(contract_id: ContractId, client: Arc<FuelClient>) -> Self {
        Self {
            contract_id,
            client,
        }
    }

    pub async fn load_contract_state(
        self,
        target_block_height: BlockHeight,
    ) -> anyhow::Result<ContractStorage> {
        let client = self.client.clone();
        let slots_task = tokio::task::spawn(async move {
            let slots_keys = client
                .contract_storage_slots(&self.contract_id)
                .await?
                .map_ok(|(key, _)| key)
                .try_collect::<Vec<_>>()
                .await?;

            let batched_keys = slots_keys
                .chunks(CHUNK_SIZE)
                .map(|b| b.to_vec())
                .collect::<Vec<_>>();

            let slots_values = futures::stream::iter(batched_keys)
                .map(|slots| {
                    let client = client.clone();

                    async move {
                        client
                            .contract_slots_values(
                                &self.contract_id,
                                Some(target_block_height),
                                slots,
                            )
                            .await
                    }
                })
                .buffered(10)
                .collect::<Vec<_>>()
                .await;

            let slots_values: Vec<_> = slots_values.into_iter().try_collect()?;
            let slots_values = slots_values.into_iter().flatten().collect::<Vec<_>>();

            Ok::<_, anyhow::Error>(slots_values)
        });

        let client = self.client.clone();
        let balances_task = tokio::task::spawn(async move {
            let asset_keys = client
                .contract_storage_balances(&self.contract_id)
                .await?
                .map_ok(|balance| AssetId::from(balance.asset_id))
                .try_collect::<Vec<_>>()
                .await?;

            let asset_values = client
                .contract_balance_values(
                    &self.contract_id,
                    Some(target_block_height),
                    asset_keys,
                )
                .await?;

            Ok::<_, anyhow::Error>(asset_values)
        });

        let (slots_result, assets_result) =
            futures::future::join(slots_task, balances_task).await;
        let (slots, assets) = (slots_result??, assets_result??);

        let contract_storage = ContractStorage::new(
            self.contract_id,
            slots,
            assets.into_iter().map(Into::into).collect(),
        );

        Ok(contract_storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuel_core_client::client::FuelClient;
    use fuel_types::ContractId;
    use std::sync::Arc;

    const URL: &str = "http://localhost:4001";

    #[tokio::test]
    async fn test_load_contract_state() {
        let client = FuelClient::new(URL).unwrap();
        let client = Arc::new(client);
        let contract_id: ContractId =
            "0x2e40f2b244b98ed6b8204b3de0156c6961f98525c8162f80162fcf53eebd90e7"
                .parse()
                .unwrap();

        let block_height = client
            .chain_info()
            .await
            .unwrap()
            .latest_block
            .header
            .height;

        let contract_state_loader = ContractStateLoader::new(contract_id, client);
        let contract_storage = contract_state_loader
            .load_contract_state(block_height.into())
            .await
            .expect("Failed to load contract state");

        assert!(contract_storage.slots().len() > 0);
        assert!(contract_storage.assets().len() > 0);
    }
}
