# Yellowstone Geyser Client

Minimal high-performance Yellowstone Geyser gRPC client for Solana account subscriptions.

Built by [Venum](https://www.venum.dev/).

This crate is the open-source, generic version of the Geyser transport layer we use internally. It is intentionally focused on one job: consume Yellowstone account streams efficiently and hand raw account updates to your application with minimal overhead.

It is designed for Yellowstone-compatible Geyser gRPC providers.

It is built on top of the official Yellowstone client and proto crates from the [`rpcpool/yellowstone-grpc`](https://github.com/rpcpool/yellowstone-grpc) repository.

## Highlights

- subscribe by owner, explicit account, or both
- optional `data_size` filtering
- automatic reconnect loop
- skip unchanged account updates with bounded dedupe
- emit raw account updates over a Tokio channel
- no framework lock-in

## Why this exists

Many Solana systems do not need a full framework to consume Geyser.

They need a small reusable building block that already handles:

- Yellowstone connection setup
- account subscription request building
- reconnects
- unchanged-account dedupe
- raw account update delivery

This crate is that building block.

If you are building a Solana system that needs lower-overhead account streams without dragging in a full application framework, this crate is meant to be a small, reusable transport layer.

## What it does

- subscribe to accounts by owner and/or explicit pubkeys
- reconnect automatically on stream errors
- dedupe unchanged account data with a bounded hash map
- emit raw account updates through a Tokio channel

## What it does not do

- no N-API bindings
- no JavaScript-specific optimization layer
- no hardcoded DEX decoders
- no strategy logic

This is meant to be a generic building block that other Solana systems can use.

## Good fit for

- bots
- market data indexers
- account-stream workers
- custom pool state pipelines
- Rust services that want a clean Geyser transport layer

## Node.js / N-API

This crate does **not** ship N-API bindings itself.

That is intentional.

The crate is pure Rust so consumers can:

- use it directly from Rust
- wrap it with their own N-API layer for Node.js
- embed it in a service process
- build their own FFI boundary if needed

In other words: the transport layer is open-sourced, but the JavaScript/native integration boundary is left to the consumer.

## Dependencies

Core upstream dependencies:

- [`yellowstone-grpc-client`](https://github.com/rpcpool/yellowstone-grpc)
- [`yellowstone-grpc-proto`](https://github.com/rpcpool/yellowstone-grpc)

These are the official Yellowstone gRPC Rust crates. This project intentionally builds on them instead of reimplementing the wire protocol.

## Install

```toml
[dependencies]
yellowstone-geyser-client = { path = "." }
```

## Example

```rust
use yellowstone_geyser_client::{spawn_geyser, AccountSubscription, GeyserConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = GeyserConfig {
        endpoint: "https://your-geyser-endpoint".into(),
        x_token: Some("token".into()),
        subscriptions: vec![AccountSubscription {
            label: "whirlpools".into(),
            owners: vec!["whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc".into()],
            accounts: vec![],
            data_size: Some(653),
        }],
        ..Default::default()
    };

    let (_handle, mut rx) = spawn_geyser(config)?;

    while let Some(update) = rx.recv().await {
        println!("slot={} filters={:?} bytes={}", update.slot, update.filters, update.data.len());
    }

    Ok(())
}
```

## Configuration Notes

- `commitment` defaults to `processed`
- `skip_unchanged_accounts` defaults to `true`
- the dedupe hash map is bounded and clears on overflow
- updates are delivered as raw account payloads; decoding is up to the consumer

## Design Notes

- commitment defaults to `processed`
- dedupe hash map is bounded and clears on overflow
- raw account bytes are preserved in the emitted update

## Status

This crate is designed to be useful on its own, but it is intentionally minimal.

Future additions may include:

- example binaries
- optional decoder helpers
- optional Node/N-API wrapper as a separate crate or package

## About Venum

Venum builds cost-efficient Solana execution infrastructure for builders, bots, and agents.

Website:

- [https://www.venum.dev/](https://www.venum.dev/)
