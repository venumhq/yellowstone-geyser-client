use std::collections::HashMap;
use std::fmt;
use std::sync::Once;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::mpsc;

use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcClient};
use yellowstone_grpc_proto::geyser::{
    subscribe_request_filter_accounts_filter::Filter as AccountFilter,
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterAccounts, SubscribeRequestFilterAccountsFilter,
};

static CRYPTO_INIT: Once = Once::new();

fn ensure_crypto() {
    CRYPTO_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Debug, Clone)]
pub struct AccountSubscription {
    pub label: String,
    pub owners: Vec<String>,
    pub accounts: Vec<String>,
    pub data_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Commitment {
    #[default]
    Processed,
    Confirmed,
    Finalized,
}

impl Commitment {
    fn as_proto(self) -> i32 {
        match self {
            Commitment::Processed => CommitmentLevel::Processed as i32,
            Commitment::Confirmed => CommitmentLevel::Confirmed as i32,
            Commitment::Finalized => CommitmentLevel::Finalized as i32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeyserConfig {
    pub endpoint: String,
    pub x_token: Option<String>,
    pub subscriptions: Vec<AccountSubscription>,
    pub commitment: Commitment,
    pub reconnect_delay_ms: u64,
    pub max_hash_entries: usize,
    pub channel_capacity: usize,
    pub skip_unchanged_accounts: bool,
}

impl Default for GeyserConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            x_token: None,
            subscriptions: Vec::new(),
            commitment: Commitment::Processed,
            reconnect_delay_ms: 5_000,
            max_hash_entries: 50_000,
            channel_capacity: 8_192,
            skip_unchanged_accounts: true,
        }
    }
}

impl GeyserConfig {
    pub fn validate(&self) -> Result<(), GeyserError> {
        if self.endpoint.trim().is_empty() {
            return Err(GeyserError::new("endpoint is required"));
        }
        if self.subscriptions.is_empty() {
            return Err(GeyserError::new("at least one subscription is required"));
        }
        if self.channel_capacity == 0 {
            return Err(GeyserError::new("channel_capacity must be greater than 0"));
        }
        if self.max_hash_entries == 0 {
            return Err(GeyserError::new("max_hash_entries must be greater than 0"));
        }
        for subscription in &self.subscriptions {
            if subscription.label.trim().is_empty() {
                return Err(GeyserError::new("subscription label is required"));
            }
            if subscription.owners.is_empty() && subscription.accounts.is_empty() {
                return Err(GeyserError::new(format!(
                    "subscription '{}' must include at least one owner or account",
                    subscription.label
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AccountUpdate {
    pub filters: Vec<String>,
    pub slot: u64,
    pub is_startup: bool,
    pub pubkey: Vec<u8>,
    pub lamports: u64,
    pub owner: Vec<u8>,
    pub executable: bool,
    pub rent_epoch: u64,
    pub write_version: u64,
    pub txn_signature: Option<Vec<u8>>,
    pub data: Vec<u8>,
}

pub struct GeyserHandle {
    stop_tx: Option<mpsc::Sender<()>>,
}

impl GeyserHandle {
    pub fn stop(&mut self) -> Result<(), GeyserError> {
        if let Some(tx) = self.stop_tx.take() {
            tx.try_send(())
                .map_err(|err| GeyserError::control(format!("stop send failed: {err}")))?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct GeyserError {
    message: String,
}

impl GeyserError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn control(message: impl Into<String>) -> Self {
        Self::new(message)
    }
}

impl fmt::Display for GeyserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GeyserError {}

pub fn spawn_geyser(
    config: GeyserConfig,
) -> Result<(GeyserHandle, mpsc::Receiver<AccountUpdate>), GeyserError> {
    config.validate()?;

    ensure_crypto();
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (update_tx, update_rx) = mpsc::channel::<AccountUpdate>(config.channel_capacity);

    tokio::spawn(async move {
        run_geyser_loop(config, update_tx, stop_rx).await;
    });

    Ok((
        GeyserHandle {
            stop_tx: Some(stop_tx),
        },
        update_rx,
    ))
}

async fn run_geyser_loop(
    config: GeyserConfig,
    update_tx: mpsc::Sender<AccountUpdate>,
    mut stop_rx: mpsc::Receiver<()>,
) {
    loop {
        match run_geyser_once(&config, &update_tx, &mut stop_rx).await {
            Ok(()) => break,
            Err(err) => {
                eprintln!(
                    "[yellowstone-geyser-client] error: {err}, reconnecting in {}ms",
                    config.reconnect_delay_ms
                );
                tokio::select! {
                    _ = stop_rx.recv() => break,
                    _ = tokio::time::sleep(Duration::from_millis(config.reconnect_delay_ms)) => {}
                }
            }
        }
    }
}

async fn run_geyser_once(
    config: &GeyserConfig,
    update_tx: &mpsc::Sender<AccountUpdate>,
    stop_rx: &mut mpsc::Receiver<()>,
) -> Result<(), GeyserError> {
    let mut client = GeyserGrpcClient::build_from_shared(config.endpoint.clone())
        .map_err(|err| GeyserError::new(format!("build client: {err}")))?
        .x_token(config.x_token.clone())
        .map_err(|err| GeyserError::new(format!("configure token: {err}")))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|err| GeyserError::new(format!("configure tls: {err}")))?
        .connect()
        .await
        .map_err(|err| GeyserError::new(format!("connect: {err:?}")))?;

    let accounts = build_accounts_map(&config.subscriptions);

    let request = SubscribeRequest {
        accounts,
        commitment: Some(config.commitment.as_proto()),
        ..Default::default()
    };

    let (_subscribe_tx, mut stream) = client
        .subscribe_with_request(Some(request))
        .await
        .map_err(|err| GeyserError::new(format!("subscribe: {err}")))?;

    let mut hashes: HashMap<Vec<u8>, u32> = HashMap::with_capacity(config.max_hash_entries);

    loop {
        tokio::select! {
            biased;
            _ = stop_rx.recv() => break,
            next = stream.next() => {
                match next {
                    Some(Ok(update)) => {
                        if let Some(UpdateOneof::Account(account_update)) = update.update_oneof {
                            if let Some(account) = account_update.account {
                                let pubkey = account.pubkey.clone();
                                if config.skip_unchanged_accounts {
                                    let hash = fnv1a(&account.data);
                                    if let Some(previous) = hashes.get(&pubkey) {
                                        if *previous == hash {
                                            continue;
                                        }
                                    }
                                    if hashes.len() >= config.max_hash_entries {
                                        hashes.clear();
                                    }
                                    hashes.insert(pubkey.clone(), hash);
                                }

                                let next_update = AccountUpdate {
                                    filters: update.filters,
                                    slot: account_update.slot,
                                    is_startup: account_update.is_startup,
                                    pubkey: pubkey.clone(),
                                    lamports: account.lamports,
                                    owner: account.owner,
                                    executable: account.executable,
                                    rent_epoch: account.rent_epoch,
                                    write_version: account.write_version,
                                    txn_signature: account.txn_signature,
                                    data: account.data,
                                };
                                update_tx
                                    .send(next_update)
                                    .await
                                    .map_err(|err| GeyserError::new(format!("emit update: {err}")))?;
                            }
                        }
                    }
                    Some(Err(err)) => return Err(GeyserError::new(format!("stream: {err}"))),
                    None => return Err(GeyserError::new("stream ended")),
                }
            }
        }
    }
    Ok(())
}

fn fnv1a(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn build_accounts_map(
    subscriptions: &[AccountSubscription],
) -> HashMap<String, SubscribeRequestFilterAccounts> {
    subscriptions
        .iter()
        .map(|subscription| {
            let mut filters = Vec::new();
            if let Some(data_size) = subscription.data_size {
                filters.push(SubscribeRequestFilterAccountsFilter {
                    filter: Some(AccountFilter::Datasize(data_size)),
                });
            }

            (
                subscription.label.clone(),
                SubscribeRequestFilterAccounts {
                    account: subscription.accounts.clone(),
                    owner: subscription.owners.clone(),
                    filters,
                    nonempty_txn_signature: None,
                },
            )
        })
        .collect::<HashMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_subscription() -> AccountSubscription {
        AccountSubscription {
            label: "whirlpools".into(),
            owners: vec!["whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc".into()],
            accounts: vec![],
            data_size: Some(653),
        }
    }

    #[test]
    fn default_config_is_sane() {
        let config = GeyserConfig::default();
        assert_eq!(config.commitment as u8, Commitment::Processed as u8);
        assert_eq!(config.reconnect_delay_ms, 5_000);
        assert_eq!(config.max_hash_entries, 50_000);
        assert_eq!(config.channel_capacity, 8_192);
        assert!(config.skip_unchanged_accounts);
    }

    #[test]
    fn validate_rejects_missing_endpoint() {
        let mut config = GeyserConfig::default();
        config.subscriptions.push(sample_subscription());
        let err = config.validate().unwrap_err();
        assert_eq!(err.to_string(), "endpoint is required");
    }

    #[test]
    fn validate_rejects_subscription_without_owner_or_account() {
        let mut config = GeyserConfig {
            endpoint: "https://example.com".into(),
            ..Default::default()
        };
        config.subscriptions.push(AccountSubscription {
            label: "invalid".into(),
            owners: vec![],
            accounts: vec![],
            data_size: None,
        });
        let err = config.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("must include at least one owner or account"));
    }

    #[test]
    fn build_accounts_map_preserves_filters_and_targets() {
        let accounts = build_accounts_map(&[sample_subscription()]);
        let whirlpools = accounts
            .get("whirlpools")
            .expect("missing whirlpools subscription");
        assert_eq!(
            whirlpools.owner,
            vec!["whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"]
        );
        assert!(whirlpools.account.is_empty());
        assert_eq!(whirlpools.filters.len(), 1);
        match whirlpools.filters[0].filter {
            Some(AccountFilter::Datasize(size)) => assert_eq!(size, 653),
            _ => panic!("expected datasize filter"),
        }
    }

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(b"hello"), 0x4f9f2cab);
        assert_ne!(fnv1a(b"hello"), fnv1a(b"hello!"));
    }
}
