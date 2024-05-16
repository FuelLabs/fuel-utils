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