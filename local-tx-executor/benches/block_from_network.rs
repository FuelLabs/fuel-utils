use criterion::{
    criterion_group,
    criterion_main,
    Criterion,
};
use fuel_core_client::client::FuelClient;
use fuel_core_types::blockchain::block::Block;
use local_tx_executor::full_block::ClientExt;

fn block_from_network(c: &mut Criterion) {
    let client = FuelClient::new("https://testnet.fuel.network/v1/graphql").unwrap();
    let height = 39432549;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let block = runtime
        .block_on(client.full_blocks(height..height + 1))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    println!("Number of transactions: {}", block.transactions().len());

    c.bench_function("block_from_network_serialize", |b| {
        b.iter(|| {
            let _ = postcard::to_allocvec(&block).unwrap();
        });
    });

    c.bench_function("block_from_network_deserialize", |b| {
        let serialized = postcard::to_allocvec(&block).unwrap();
        b.iter(|| {
            let _: Block = postcard::from_bytes(&serialized).unwrap();
        });
    });
}

criterion_group!(benches, block_from_network);
criterion_main!(benches);

// If you want to debug the benchmarks, you can run them with code below:
// But first you need to comment `criterion_group` and `criterion_main` macros above.
//
// fn main() {
//     let criterion = Criterion::default();
//     let mut criterion = criterion.with_filter("tx_from_network");
//     tx_from_network(&mut criterion);
// }
//
// #[test]
// fn dummy() {}
