use fuel_core_client::client::{
    types::ContractBalance,
    FuelClient,
};
use fuel_types::{
    AssetId,
    BlockHeight,
    Bytes32,
    ContractId,
    Word,
};
use fuel_vm::storage::ContractsStateData;
use std::collections::HashMap;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ContractStorage {
    contract_id: ContractId,
    slots: HashMap<Bytes32, Option<ContractsStateData>>,
    assets: HashMap<AssetId, Option<Word>>,
}

impl ContractStorage {
    pub fn new(
        contract_id: ContractId,
        slots: Vec<(Bytes32, Vec<u8>)>,
        assets: Vec<ContractBalance>,
    ) -> Self {
        Self {
            contract_id,
            slots: slots
                .into_iter()
                .map(|(k, v)| (k, Some(v.into())))
                .collect(),
            assets: assets
                .into_iter()
                .map(|balance| (balance.asset_id, Some(balance.amount)))
                .collect(),
        }
    }

    pub fn slots(&self) -> &HashMap<Bytes32, Option<ContractsStateData>> {
        &self.slots
    }

    pub fn assets(&self) -> &HashMap<AssetId, Option<Word>> {
        &self.assets
    }

    pub async fn slot<'a>(
        &'a mut self,
        key: &Bytes32,
        block_height: &BlockHeight,
        client: &FuelClient,
    ) -> anyhow::Result<Option<&'a ContractsStateData>> {
        if !self.slots.contains_key(key) {
            tracing::warn!(
                "Fetching asset value from the network for the contract {}.",
                self.contract_id
            );
            let fetched_value = client
                .contract_slots_values(&self.contract_id, Some(*block_height), vec![*key])
                .await?
                .into_iter()
                .next();
            let fetched_value = fetched_value.map(|(_, v)| v.into());
            self.slots.insert(*key, fetched_value);
        }

        Ok(self.slots.get(key).expect("We checked above; qed").as_ref())
    }

    pub async fn asset(
        &mut self,
        key: &AssetId,
        block_height: &BlockHeight,
        client: &FuelClient,
    ) -> anyhow::Result<Option<Word>> {
        let value = self.assets.get(key);

        match value {
            Some(Some(value)) => Ok(Some(*value)),
            None => {
                tracing::warn!(
                    "Fetching asset value from the network for the contract {}.",
                    self.contract_id
                );
                let value = client
                    .contract_balance_values(
                        &self.contract_id,
                        Some(*block_height),
                        vec![*key],
                    )
                    .await?
                    .into_iter()
                    .next();
                let value = value.map(|v| v.amount);
                self.assets.insert(*key, value.map(|v| v.0));
                Ok(self
                    .assets
                    .get(key)
                    .expect("We inserted value above; qed")
                    .clone())
            }
            Some(None) => Ok(None),
        }
    }

    pub fn insert_slot(&mut self, key: Bytes32, value: Option<ContractsStateData>) {
        self.slots.insert(key, value);
    }

    pub fn insert_asset(&mut self, key: AssetId, value: Option<Word>) {
        self.assets.insert(key, value);
    }
}
