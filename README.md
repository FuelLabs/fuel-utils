# Util to interact with the network

```shell
Usage: fuel-core-network-upgrader <COMMAND>

Commands:
  upgrade     Upgrades the state transition function of the network
  upload      Uploads the state transition bytecode
  transfer    Transfers assets to recipient
  parameters  Downloads consensus parameters from the node
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### Transfer
```shell
Usage: fuel-core-network-upgrader transfer [OPTIONS] --amount <AMOUNT> --recipient <RECIPIENT> --url <URL>

Options:
  -a, --amount <AMOUNT>        The amount to transfer [env: AMOUNT=]
  -r, --recipient <RECIPIENT>  The recipient of the transfer [env: RECIPIENT=]
  -u, --url <URL>              The URL to upload the bytecode to [env: URL=https://devnet.fuel.network/v1/graphql]
  -i, --asset-id <ASSET_ID>    The asset to transfer [env: ASSET_ID=]
  -h, --help                   Print help
```

### Upload

```shell
Usage: fuel-core-network-upgrader upload [OPTIONS] --path <PATH> --url <URL>

Options:
  -p, --path <PATH>
          The path to the bytecode file to upload
  -u, --url <URL>
          The URL to upload the bytecode to [env: URL=https://devnet.fuel.network/v1/graphql]
      --subsection-size <SUBSECTION_SIZE>
          The size of the subsections to split the bytecode into. Default size is 64KB [env: SUBSECTION_SIZE=] [default: 65536]
  -s, --starting-subsection <STARTING_SUBSECTION>
          The size of the subsections to split the bytecode into. Default size is 64KB [env: STARTING_SUBSECTION=] [default: 0]
  -h, --help
          Print help
```

### Upgrade

```shell
Usage: fuel-core-network-upgrader upgrade --url <URL> <COMMAND>

Commands:
  state-transition      Upgrades the state transition function of the network
  consensus-parameters  Upgrades the consensus parameters of the network
  help                  Print this message or the help of the given subcommand(s)

Options:
  -u, --url <URL>  The URL to upload the bytecode to [env: URL=https://devnet.fuel.network/v1/graphql]
  -h, --help       Print help

```

### Parameters

```shell
Usage: fuel-core-network-upgrader parameters [OPTIONS] --url <URL>

Options:
  -p, --path <PATH>  The path to dump the consensus parameters [default: consensus_parameters.json]
  -u, --url <URL>    The URL to upload the bytecode to [env: URL=https://devnet.fuel.network/v1/graphql]
  -h, --help         Print help
```

## Update consensus parameters

1. Download the current consensus parameters from the node:
```shell
URL=https://devnet.fuel.network/v1/graphql fuel-core-network-upgrader parameters
```
2. Update the `consensus_parameters.json` file with the new parameters.
3. Upload the new consensus parameters to the node by using Privileged Address account with funds:
```shell
URL=https://devnet.fuel.network/v1/graphql fuel-core-network-upgrader upgrade consensus-parameters -p consensus_parameters.json
```

## Update state transition function

1. Upload the new state transition bytecode to the node by using ANY account with funds:
```shell
URL=https://devnet.fuel.network/v1/graphql fuel-core-network-upgrader upload -p fuel-core-wasm-executor.wasm
```
2. Copy the root of the bytecode from the logs.
3. Upgrade to uploaded state transition function by using Privileged Address account with funds:
```shell
URL=https://devnet.fuel.network/v1/graphql fuel-core-network-upgrader upgrade state-transition -r 0000000000000000000000000000000000000000000000000000000000000000
```

## Transfer assets

You can transfer assets to any account by using ANY account with funds:
```shell
URL=https://devnet.fuel.network/v1/graphql fuel-core-network-upgrader transfer --amount 10000000000 --recipient 1d5adf1197c4f48dfa642261e98fcefedd2ccd805dbbd2c574ddf3ed6dc66b5a
```