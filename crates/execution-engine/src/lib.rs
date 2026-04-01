use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::Utc;
use config_core::PolymarketExecutionConfig;
use domain_core::{
    Event, ManagedOrder, Market, OrderIntent, OrderIntentAction, OrderStatus, Position,
    PositionSide, RiskDecision, SignalSide, TradeSignal,
};
use futures_util::StreamExt;
use polymarket_client_sdk::POLYGON;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::{Credentials, LocalSigner, Normal, Signer};
use polymarket_client_sdk::clob::types::request::{
    BalanceAllowanceRequest, OrdersRequest, TradesRequest,
};
use polymarket_client_sdk::clob::types::{
    AssetType, OrderStatusType, OrderType, Side, SignatureType,
};
use polymarket_client_sdk::clob::ws::{Client as WsClient, OrderMessage, TradeMessage};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::{Address, B256, Decimal, U256};
use std::str::FromStr;
use std::sync::Arc;
use storage_rocksdb::Storage;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[async_trait]
pub trait ExecutionGateway {
    async fn submit(&self, market: &Market, order: ManagedOrder) -> Result<ManagedOrder>;
    async fn cancel_local_first(&self, local_order: &ManagedOrder) -> Result<CancelReport>;
}

#[derive(Clone)]
pub struct RecordingExecutionGateway {
    storage: Storage,
}

impl RecordingExecutionGateway {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl ExecutionGateway for RecordingExecutionGateway {
    async fn submit(&self, _market: &Market, order: ManagedOrder) -> Result<ManagedOrder> {
        self.storage.save_order(&order)?;
        self.storage
            .append_event(&Event::OrderRecorded(order.clone()))?;
        Ok(order)
    }

    async fn cancel_local_first(&self, local_order: &ManagedOrder) -> Result<CancelReport> {
        let mut cancelled = local_order.clone();
        cancelled.status = OrderStatus::Cancelled;
        cancelled.remaining_shares = 0.0;
        cancelled.last_update = Utc::now();
        self.storage.save_order(&cancelled)?;
        self.storage
            .append_event(&Event::OrderCancelled(cancelled.intent.id.to_string()))?;
        Ok(CancelReport {
            canceled_count: 1,
            not_canceled_count: 0,
        })
    }
}

pub struct VenueHealthReport {
    pub country: String,
    pub blocked: bool,
    pub api_key_count: usize,
    pub open_orders: usize,
    pub recent_trades: usize,
    pub collateral_balance: String,
    pub collateral_allowance_entries: usize,
}

pub struct ReconciliationReport {
    pub total_local_orders: usize,
    pub updated_orders: usize,
    pub filled_orders: usize,
    pub cancelled_orders: usize,
    pub recent_trades: usize,
}

pub struct CancelReport {
    pub canceled_count: usize,
    pub not_canceled_count: usize,
}

pub struct RequoteReport {
    pub canceled_orders: usize,
    pub reposted_orders: usize,
}

pub struct RecoveryReport {
    pub pending_cancel_orders: usize,
    pub pending_replace_orders: usize,
    pub remote_cancel_attempts: usize,
    pub local_state_fixes: usize,
    pub replacement_submissions: usize,
}

#[derive(Clone)]
pub struct PolymarketExecutionGateway {
    storage: Storage,
    clob_url: String,
    private_key: String,
    funder_address: Option<String>,
    signature_type: SignatureType,
}

impl PolymarketExecutionGateway {
    pub async fn connect(config: &PolymarketExecutionConfig, storage: Storage) -> Result<Self> {
        let private_key = config
            .private_key
            .as_ref()
            .context("live trading requires POLYMARKET_PRIVATE_KEY")?;
        let gateway = Self {
            storage,
            clob_url: config.clob_url.clone(),
            private_key: private_key.clone(),
            funder_address: config.funder_address.clone(),
            signature_type: signature_type_from_u8(config.signature_type)?,
        };
        let report = gateway.health_report().await?;
        if report.blocked {
            bail!(
                "Polymarket geoblock check failed for country {}",
                report.country
            );
        }
        Ok(gateway)
    }

    pub async fn health_report(&self) -> Result<VenueHealthReport> {
        let public_client = self.public_client()?;
        let client = self.authenticated_client().await?;
        let geoblock = public_client
            .check_geoblock()
            .await
            .context("failed to query Polymarket geoblock status")?;
        let api_keys = client
            .api_keys()
            .await
            .context("failed to list Polymarket API keys")?;
        let open_orders = client
            .orders(&OrdersRequest::default(), None)
            .await
            .context("failed to query open orders from Polymarket")?;
        let recent_trades = client
            .trades(&TradesRequest::default(), None)
            .await
            .context("failed to query trades from Polymarket")?;
        let balance_request = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .signature_type(self.signature_type)
            .build();
        let collateral = client
            .balance_allowance(balance_request)
            .await
            .context("failed to query Polymarket collateral balance/allowance")?;

        Ok(VenueHealthReport {
            country: geoblock.country,
            blocked: geoblock.blocked,
            api_key_count: format!("{api_keys:?}").matches("ApiKey").count(),
            open_orders: open_orders.data.len(),
            recent_trades: recent_trades.data.len(),
            collateral_balance: collateral.balance.to_string(),
            collateral_allowance_entries: collateral.allowances.len(),
        })
    }

    pub async fn reconcile_orders(&self) -> Result<ReconciliationReport> {
        let client = self.authenticated_client().await?;
        let local_orders = self.storage.list_orders()?;
        let recent_trades = client
            .trades(&TradesRequest::default(), None)
            .await
            .context("failed to query recent Polymarket trades during reconciliation")?;

        let mut updated_orders = 0usize;
        let mut filled_orders = 0usize;
        let mut cancelled_orders = 0usize;

        for mut local_order in local_orders.iter().cloned() {
            let Some(venue_order_id) = local_order.venue_order_id.clone() else {
                continue;
            };

            let remote_order = client
                .order(&venue_order_id)
                .await
                .with_context(|| format!("failed to query remote order {venue_order_id}"))?;
            let mapped_status = map_remote_order_status(remote_order.status);
            let matched_shares = decimal_to_f64(&remote_order.size_matched)?;
            let original_shares = decimal_to_f64(&remote_order.original_size)?;
            let remaining_shares = (original_shares - matched_shares).max(0.0);

            if mapped_status != local_order.status
                || (local_order.filled_shares - matched_shares).abs() > 1e-9
                || (local_order.remaining_shares - remaining_shares).abs() > 1e-9
            {
                local_order.status = mapped_status;
                local_order.filled_shares = matched_shares;
                local_order.remaining_shares = remaining_shares;
                local_order.last_update = Utc::now();
                self.storage.save_order(&local_order)?;
                self.storage
                    .append_event(&Event::OrderUpdated(local_order.clone()))?;
                updated_orders += 1;
            }

            if matches!(mapped_status, OrderStatus::Filled) {
                filled_orders += 1;
            }
            if matches!(mapped_status, OrderStatus::Cancelled) {
                cancelled_orders += 1;
            }
        }

        Ok(ReconciliationReport {
            total_local_orders: local_orders.len(),
            updated_orders,
            filled_orders,
            cancelled_orders,
            recent_trades: recent_trades.data.len(),
        })
    }

    pub async fn recover_pending_orders(&self) -> Result<RecoveryReport> {
        let client = self.authenticated_client().await?;
        let local_orders = self.storage.list_orders()?;
        let markets = self.storage.list_markets()?;

        let mut pending_cancel_orders = 0usize;
        let mut pending_replace_orders = 0usize;
        let mut remote_cancel_attempts = 0usize;
        let mut local_state_fixes = 0usize;
        let mut replacement_submissions = 0usize;

        for local_order in &local_orders {
            match local_order.status {
                OrderStatus::PendingCancel => {
                    pending_cancel_orders += 1;
                    let fixed = self
                        .recover_pending_cancel(&client, local_order)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to recover pending cancel order {}",
                                local_order.intent.id
                            )
                        })?;
                    remote_cancel_attempts += fixed.remote_cancel_attempts;
                    local_state_fixes += fixed.local_state_fixes;
                }
                OrderStatus::PendingReplace => {
                    pending_replace_orders += 1;
                    let replacement = find_unsent_replacement(local_order, &local_orders);
                    let fixed = self
                        .recover_pending_replace(&client, local_order, replacement, &markets)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to recover pending replace order {}",
                                local_order.intent.id
                            )
                        })?;
                    remote_cancel_attempts += fixed.remote_cancel_attempts;
                    local_state_fixes += fixed.local_state_fixes;
                    replacement_submissions += fixed.replacement_submissions;
                }
                _ => {}
            }
        }

        for replacement in local_orders.iter().filter(|order| {
            order.replacement_for.is_some()
                && order.venue_order_id.is_none()
                && matches!(order.status, OrderStatus::Recorded)
        }) {
            let Some(parent_id) = replacement.replacement_for else {
                continue;
            };
            let Some(parent) = local_orders
                .iter()
                .find(|order| order.intent.id == parent_id)
            else {
                continue;
            };
            if !matches!(parent.status, OrderStatus::Cancelled | OrderStatus::Filled) {
                continue;
            }
            let Some(market) = markets
                .iter()
                .find(|market| market.market_id == replacement.intent.market_id)
            else {
                continue;
            };
            self.submit(market, replacement.clone()).await?;
            if matches!(replacement.intent.action, OrderIntentAction::Close) {
                reserve_pending_close(&self.storage, replacement)?;
            }
            replacement_submissions += 1;
        }

        Ok(RecoveryReport {
            pending_cancel_orders,
            pending_replace_orders,
            remote_cancel_attempts,
            local_state_fixes,
            replacement_submissions,
        })
    }

    pub async fn cancel_order(&self, venue_order_id: &str) -> Result<CancelReport> {
        let client = self.authenticated_client().await?;
        let response = client
            .cancel_order(venue_order_id)
            .await
            .with_context(|| format!("failed to cancel remote order {venue_order_id}"))?;
        self.mark_local_orders_cancelled(&response.canceled)?;
        Ok(CancelReport {
            canceled_count: response.canceled.len(),
            not_canceled_count: response.not_canceled.len(),
        })
    }

    pub async fn cancel_all_orders(&self) -> Result<CancelReport> {
        let client = self.authenticated_client().await?;
        let response = client
            .cancel_all_orders()
            .await
            .context("failed to cancel all remote orders")?;
        self.mark_local_orders_cancelled(&response.canceled)?;
        Ok(CancelReport {
            canceled_count: response.canceled.len(),
            not_canceled_count: response.not_canceled.len(),
        })
    }

    pub async fn cancel_order_local_first(
        &self,
        local_order: &ManagedOrder,
    ) -> Result<CancelReport> {
        let Some(venue_order_id) = local_order.venue_order_id.as_deref() else {
            bail!("cannot cancel local order without venue_order_id");
        };
        let mut pending_cancel = local_order.clone();
        pending_cancel.status = OrderStatus::PendingCancel;
        pending_cancel.last_update = Utc::now();
        self.storage.save_order(&pending_cancel)?;
        self.storage
            .append_event(&Event::OrderUpdated(pending_cancel))?;
        self.cancel_order(venue_order_id).await
    }

    pub async fn spawn_user_stream_sync(&self) -> Result<Vec<JoinHandle<()>>> {
        let signer = LocalSigner::from_str(&self.private_key)
            .context("failed to parse POLYMARKET_PRIVATE_KEY")?
            .with_chain_id(Some(POLYGON));
        let credentials = self.ws_credentials().await?;
        let address = signer.address();
        let markets = self.subscribed_condition_ids()?;
        if markets.is_empty() {
            return Ok(Vec::new());
        }

        let mut handles = Vec::new();

        let orders_storage = self.storage.clone();
        let orders_endpoint = self.clob_url.clone();
        let orders_credentials = credentials.clone();
        let orders_address = address;
        let order_markets = markets.clone();
        handles.push(tokio::spawn(async move {
            if let Err(error) = run_order_stream_loop(
                orders_storage,
                orders_endpoint,
                orders_credentials,
                orders_address,
                order_markets,
            )
            .await
            {
                tracing::warn!(error = %error, "order websocket sync loop exited");
            }
        }));

        let trades_storage = self.storage.clone();
        let trades_endpoint = self.clob_url.clone();
        let trades_credentials = credentials;
        let trades_address = address;
        handles.push(tokio::spawn(async move {
            if let Err(error) = run_trade_stream_loop(
                trades_storage,
                trades_endpoint,
                trades_credentials,
                trades_address,
                markets,
            )
            .await
            {
                tracing::warn!(error = %error, "trade websocket sync loop exited");
            }
        }));

        Ok(handles)
    }

    fn public_client(&self) -> Result<Client> {
        Client::new(&self.clob_url, Config::default())
            .context("failed to create Polymarket public client")
    }

    async fn authenticated_client(&self) -> Result<Client<Authenticated<Normal>>> {
        let signer = LocalSigner::from_str(&self.private_key)
            .context("failed to parse POLYMARKET_PRIVATE_KEY")?
            .with_chain_id(Some(POLYGON));
        let base_client = self.public_client()?;
        let builder = base_client.authentication_builder(&signer);
        let builder = if let Some(funder_address) = self.funder_address.as_ref() {
            let funder = Address::from_str(funder_address).with_context(|| {
                format!("failed to parse POLYMARKET_FUNDER_ADDRESS={funder_address}")
            })?;
            builder.funder(funder).signature_type(self.signature_type)
        } else {
            builder.signature_type(self.signature_type)
        };

        builder
            .authenticate()
            .await
            .context("failed to authenticate Polymarket execution client")
    }

    async fn ws_credentials(&self) -> Result<Credentials> {
        let client = self.authenticated_client().await?;
        Ok(client.credentials().clone())
    }

    fn subscribed_condition_ids(&self) -> Result<Vec<B256>> {
        let mut ids = Vec::new();
        for market in self.storage.list_markets()? {
            let Some(condition_id) = market.condition_id.as_ref() else {
                continue;
            };
            if let Ok(parsed) = B256::from_str(condition_id) {
                ids.push(parsed);
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn mark_local_orders_cancelled(&self, canceled_venue_ids: &[String]) -> Result<()> {
        if canceled_venue_ids.is_empty() {
            return Ok(());
        }

        for mut local_order in self.storage.list_orders()? {
            let Some(venue_order_id) = local_order.venue_order_id.clone() else {
                continue;
            };
            if canceled_venue_ids.iter().any(|id| id == &venue_order_id) {
                if matches!(local_order.intent.action, OrderIntentAction::Close) {
                    release_pending_close_shares(
                        &self.storage,
                        &local_order.intent.market_id,
                        local_order.intent.side,
                        local_order.remaining_shares,
                    )?;
                }
                local_order.status = OrderStatus::Cancelled;
                local_order.remaining_shares = 0.0;
                local_order.last_update = Utc::now();
                self.storage.save_order(&local_order)?;
                self.storage
                    .append_event(&Event::OrderCancelled(venue_order_id))?;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ExecutionGateway for PolymarketExecutionGateway {
    async fn submit(&self, market: &Market, mut order: ManagedOrder) -> Result<ManagedOrder> {
        let signer = LocalSigner::from_str(&self.private_key)
            .context("failed to parse POLYMARKET_PRIVATE_KEY")?
            .with_chain_id(Some(POLYGON));
        let client = self.authenticated_client().await?;
        let token_id = token_id_for_order(market, order.intent.side)?;
        let price = Decimal::from_str(&format!("{:.4}", order.intent.limit_price))
            .context("failed to convert limit price to Decimal")?;
        let share_size = if order.intent.limit_price <= 0.0 {
            bail!("limit price must be positive");
        } else {
            order.intent.quantity_shares
        };
        let size = Decimal::from_str(&format!("{:.4}", share_size))
            .context("failed to convert share size to Decimal")?;

        let signable_order = client
            .limit_order()
            .token_id(token_id)
            .side(Side::Buy)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .post_only(order.intent.maker_only)
            .build()
            .await
            .context("failed to build Polymarket limit order")?;
        let signed_order = client
            .sign(&signer, signable_order)
            .await
            .context("failed to sign Polymarket order")?;
        let response = client
            .post_order(signed_order)
            .await
            .context("failed to post Polymarket order")?;

        order.status = OrderStatus::Sent;
        order.filled_shares = 0.0;
        order.remaining_shares = order.intent.quantity_shares;
        order.venue_order_id = Some(response.order_id);
        order.last_update = Utc::now();

        let previously_recorded = self
            .storage
            .list_orders()?
            .iter()
            .any(|existing| existing.intent.id == order.intent.id);
        self.storage.save_order(&order)?;
        if previously_recorded {
            self.storage
                .append_event(&Event::OrderUpdated(order.clone()))?;
        } else {
            self.storage
                .append_event(&Event::OrderRecorded(order.clone()))?;
        }
        Ok(order)
    }

    async fn cancel_local_first(&self, local_order: &ManagedOrder) -> Result<CancelReport> {
        self.cancel_order_local_first(local_order).await
    }
}

pub struct ExecutionEngine {
    gateway: Arc<dyn ExecutionGateway + Send + Sync>,
    storage: Storage,
}

impl ExecutionEngine {
    pub fn new(gateway: Arc<dyn ExecutionGateway + Send + Sync>, storage: Storage) -> Self {
        Self { gateway, storage }
    }

    pub async fn execute(
        &self,
        market: &Market,
        signal: &TradeSignal,
        risk: &RiskDecision,
        positions: &[Position],
    ) -> Result<Option<ManagedOrder>> {
        if !risk.approved {
            return Ok(None);
        }

        let (side, action, limit_price, quantity_shares, size_usd) = match signal.side {
            SignalSide::BuyYes => {
                let price = market.best_ask.unwrap_or(0.5);
                (
                    PositionSide::Yes,
                    OrderIntentAction::Open,
                    price,
                    risk.capped_size_usd / price,
                    risk.capped_size_usd,
                )
            }
            SignalSide::BuyNo => {
                let price = 1.0 - market.best_bid.unwrap_or(0.5);
                (
                    PositionSide::No,
                    OrderIntentAction::Open,
                    price,
                    risk.capped_size_usd / price,
                    risk.capped_size_usd,
                )
            }
            SignalSide::Exit => {
                let position = positions
                    .iter()
                    .find(|position| {
                        position.market_id == market.market_id && position.exit_reason.is_none()
                    })
                    .context("exit signal received without an open position")?;
                let available_close_shares =
                    (position.quantity_shares - position.pending_close_shares).max(0.0);
                if available_close_shares <= 1e-9 {
                    return Ok(None);
                }
                let close_price = match position.side {
                    PositionSide::Yes => market.best_bid.unwrap_or(position.average_price),
                    PositionSide::No => {
                        1.0 - market.best_ask.unwrap_or(1.0 - position.average_price)
                    }
                };
                (
                    position.side,
                    OrderIntentAction::Close,
                    close_price,
                    available_close_shares,
                    available_close_shares * close_price,
                )
            }
            SignalSide::Hold => return Ok(None),
        };

        let order = ManagedOrder {
            client_intent_id: Uuid::new_v4(),
            intent: OrderIntent {
                id: Uuid::new_v4(),
                market_id: market.market_id.clone(),
                created_at: Utc::now(),
                side,
                action,
                quantity_shares,
                limit_price,
                size_usd,
                maker_only: true,
                tif: "GTC".to_string(),
            },
            status: OrderStatus::Recorded,
            filled_shares: 0.0,
            remaining_shares: quantity_shares,
            replacement_for: None,
            venue_order_id: None,
            last_update: Utc::now(),
        };

        let submitted = self.gateway.submit(market, order).await?;
        if matches!(submitted.intent.action, OrderIntentAction::Close) {
            reserve_pending_close(&self.storage, &submitted)?;
        }
        Ok(Some(submitted))
    }

    pub async fn reprice_stale_orders(
        &self,
        market: &Market,
        threshold_bps: i64,
    ) -> Result<RequoteReport> {
        let mut canceled_orders = 0usize;
        let mut reposted_orders = 0usize;
        let local_orders = self.storage.list_orders()?;

        for local_order in local_orders {
            if local_order.intent.market_id != market.market_id {
                continue;
            }
            if !matches!(local_order.status, OrderStatus::Sent) {
                continue;
            }
            if local_order.remaining_shares <= 1e-9 {
                continue;
            }

            let desired_price = desired_limit_price(market, &local_order.intent);
            let drift_bps =
                (((desired_price - local_order.intent.limit_price).abs()) * 10_000.0) as i64;
            if drift_bps < threshold_bps {
                continue;
            }

            // Local state is single-writer authoritative; once a stale order is identified,
            // cancel first and only repost the remaining shares after the venue confirms cancel.
            if local_order.venue_order_id.is_some() {
                let mut pending_replace = local_order.clone();
                pending_replace.status = OrderStatus::PendingReplace;
                pending_replace.last_update = Utc::now();
                self.storage.save_order(&pending_replace)?;
                self.storage
                    .append_event(&Event::OrderUpdated(pending_replace.clone()))?;

                let replacement = ManagedOrder {
                    client_intent_id: local_order.client_intent_id,
                    intent: OrderIntent {
                        id: Uuid::new_v4(),
                        market_id: local_order.intent.market_id.clone(),
                        created_at: Utc::now(),
                        side: local_order.intent.side,
                        action: local_order.intent.action,
                        quantity_shares: local_order.remaining_shares,
                        limit_price: desired_price,
                        size_usd: local_order.remaining_shares * desired_price,
                        maker_only: local_order.intent.maker_only,
                        tif: local_order.intent.tif.clone(),
                    },
                    status: OrderStatus::Recorded,
                    filled_shares: 0.0,
                    remaining_shares: local_order.remaining_shares,
                    replacement_for: Some(local_order.intent.id),
                    venue_order_id: None,
                    last_update: Utc::now(),
                };
                self.storage.save_order(&replacement)?;
                self.storage
                    .append_event(&Event::OrderRecorded(replacement.clone()))?;

                self.gateway.cancel_local_first(&local_order).await?;
                canceled_orders += 1;

                let submitted = self.gateway.submit(market, replacement).await?;
                if matches!(submitted.intent.action, OrderIntentAction::Close) {
                    reserve_pending_close(&self.storage, &submitted)?;
                }
                reposted_orders += 1;
            }
        }

        Ok(RequoteReport {
            canceled_orders,
            reposted_orders,
        })
    }
}

fn signature_type_from_u8(raw: u8) -> Result<SignatureType> {
    match raw {
        0 => Ok(SignatureType::Eoa),
        1 => Ok(SignatureType::Proxy),
        2 => Ok(SignatureType::GnosisSafe),
        other => bail!("unsupported POLYMARKET_SIGNATURE_TYPE={other}; expected 0, 1, or 2"),
    }
}

fn token_id_for_order(market: &Market, side: PositionSide) -> Result<U256> {
    let token = match side {
        PositionSide::Yes => market
            .yes_token_id
            .as_ref()
            .context("market missing yes_token_id for live execution")?,
        PositionSide::No => market
            .no_token_id
            .as_ref()
            .context("market missing no_token_id for live execution")?,
    };
    U256::from_str(token).with_context(|| format!("failed to parse market token id {token}"))
}

fn map_remote_order_status(status: OrderStatusType) -> OrderStatus {
    match status {
        OrderStatusType::Live | OrderStatusType::Delayed => OrderStatus::Sent,
        OrderStatusType::Matched => OrderStatus::Filled,
        OrderStatusType::Canceled => OrderStatus::Cancelled,
        OrderStatusType::Unmatched => OrderStatus::Rejected,
        _ => OrderStatus::Sent,
    }
}

struct RecoveryOutcome {
    remote_cancel_attempts: usize,
    local_state_fixes: usize,
    replacement_submissions: usize,
}

impl RecoveryOutcome {
    fn local_only() -> Self {
        Self {
            remote_cancel_attempts: 0,
            local_state_fixes: 1,
            replacement_submissions: 0,
        }
    }
}

fn desired_limit_price(market: &Market, intent: &OrderIntent) -> f64 {
    match (intent.action, intent.side) {
        (OrderIntentAction::Open, PositionSide::Yes) => {
            market.best_ask.unwrap_or(intent.limit_price)
        }
        (OrderIntentAction::Open, PositionSide::No) => {
            1.0 - market.best_bid.unwrap_or(1.0 - intent.limit_price)
        }
        (OrderIntentAction::Close, PositionSide::Yes) => {
            market.best_bid.unwrap_or(intent.limit_price)
        }
        (OrderIntentAction::Close, PositionSide::No) => {
            1.0 - market.best_ask.unwrap_or(1.0 - intent.limit_price)
        }
    }
}

async fn run_order_stream_loop(
    storage: Storage,
    endpoint: String,
    credentials: Credentials,
    address: Address,
    markets: Vec<B256>,
) -> Result<()> {
    let client = WsClient::new(&endpoint, Default::default())
        .context("failed to create Polymarket websocket client")?
        .authenticate(credentials, address)
        .context("failed to authenticate Polymarket websocket client")?;
    let mut stream = Box::pin(
        client
            .subscribe_orders(markets)
            .context("failed to subscribe to Polymarket order stream")?,
    );

    while let Some(item) = stream.next().await {
        match item {
            Ok(message) => apply_order_message(&storage, message)?,
            Err(error) => tracing::warn!(error = %error, "order stream item failed"),
        }
    }

    Ok(())
}

async fn run_trade_stream_loop(
    storage: Storage,
    endpoint: String,
    credentials: Credentials,
    address: Address,
    markets: Vec<B256>,
) -> Result<()> {
    let client = WsClient::new(&endpoint, Default::default())
        .context("failed to create Polymarket websocket client")?
        .authenticate(credentials, address)
        .context("failed to authenticate Polymarket websocket client")?;
    let mut stream = Box::pin(
        client
            .subscribe_trades(markets)
            .context("failed to subscribe to Polymarket trade stream")?,
    );

    while let Some(item) = stream.next().await {
        match item {
            Ok(message) => apply_trade_message(&storage, message)?,
            Err(error) => tracing::warn!(error = %error, "trade stream item failed"),
        }
    }

    Ok(())
}

fn apply_order_message(storage: &Storage, message: OrderMessage) -> Result<()> {
    for mut local_order in storage.list_orders()? {
        let Some(venue_order_id) = local_order.venue_order_id.clone() else {
            continue;
        };
        if venue_order_id != message.id {
            continue;
        }

        let next_status = match message.status {
            Some(OrderStatusType::Live) | Some(OrderStatusType::Delayed) => OrderStatus::Sent,
            Some(OrderStatusType::Matched) => OrderStatus::Filled,
            Some(OrderStatusType::Canceled) => OrderStatus::Cancelled,
            Some(OrderStatusType::Unmatched) => OrderStatus::Rejected,
            _ => OrderStatus::Sent,
        };

        if local_order.status != next_status {
            local_order.status = next_status;
            local_order.last_update = Utc::now();
            storage.save_order(&local_order)?;
            storage.append_event(&Event::OrderUpdated(local_order))?;
        }
    }

    Ok(())
}

impl PolymarketExecutionGateway {
    async fn recover_pending_cancel(
        &self,
        client: &Client<Authenticated<Normal>>,
        local_order: &ManagedOrder,
    ) -> Result<RecoveryOutcome> {
        let Some(venue_order_id) = local_order.venue_order_id.as_ref() else {
            finalize_local_cancellation(&self.storage, local_order, true)?;
            return Ok(RecoveryOutcome::local_only());
        };

        let remote_order = client
            .order(venue_order_id)
            .await
            .with_context(|| format!("failed to query remote order {venue_order_id}"))?;
        let mapped_status = map_remote_order_status(remote_order.status);
        let matched_shares = decimal_to_f64(&remote_order.size_matched)?;
        let original_shares = decimal_to_f64(&remote_order.original_size)?;
        let remaining_shares = (original_shares - matched_shares).max(0.0);

        match mapped_status {
            OrderStatus::Sent => {
                self.cancel_order(venue_order_id).await?;
                Ok(RecoveryOutcome {
                    remote_cancel_attempts: 1,
                    local_state_fixes: 1,
                    replacement_submissions: 0,
                })
            }
            OrderStatus::Cancelled => {
                finalize_local_cancellation(&self.storage, local_order, true)?;
                Ok(RecoveryOutcome::local_only())
            }
            _ => {
                sync_local_order(
                    &self.storage,
                    local_order,
                    mapped_status,
                    matched_shares,
                    remaining_shares,
                )?;
                Ok(RecoveryOutcome::local_only())
            }
        }
    }

    async fn recover_pending_replace(
        &self,
        client: &Client<Authenticated<Normal>>,
        local_order: &ManagedOrder,
        replacement: Option<&ManagedOrder>,
        markets: &[Market],
    ) -> Result<RecoveryOutcome> {
        let Some(venue_order_id) = local_order.venue_order_id.as_ref() else {
            finalize_local_cancellation(&self.storage, local_order, true)?;
            return self
                .submit_replacement_if_ready(local_order, replacement, markets)
                .await;
        };

        let remote_order = client
            .order(venue_order_id)
            .await
            .with_context(|| format!("failed to query remote order {venue_order_id}"))?;
        let mapped_status = map_remote_order_status(remote_order.status);
        let matched_shares = decimal_to_f64(&remote_order.size_matched)?;
        let original_shares = decimal_to_f64(&remote_order.original_size)?;
        let remaining_shares = (original_shares - matched_shares).max(0.0);

        match mapped_status {
            OrderStatus::Sent => {
                self.cancel_order(venue_order_id).await?;
                let mut outcome = self
                    .submit_replacement_if_ready(local_order, replacement, markets)
                    .await?;
                outcome.remote_cancel_attempts += 1;
                outcome.local_state_fixes += 1;
                Ok(outcome)
            }
            OrderStatus::Cancelled => {
                finalize_local_cancellation(&self.storage, local_order, true)?;
                self.submit_replacement_if_ready(local_order, replacement, markets)
                    .await
            }
            _ => {
                sync_local_order(
                    &self.storage,
                    local_order,
                    mapped_status,
                    matched_shares,
                    remaining_shares,
                )?;
                Ok(RecoveryOutcome::local_only())
            }
        }
    }

    async fn submit_replacement_if_ready(
        &self,
        local_order: &ManagedOrder,
        replacement: Option<&ManagedOrder>,
        markets: &[Market],
    ) -> Result<RecoveryOutcome> {
        let Some(replacement) = replacement else {
            return Ok(RecoveryOutcome::local_only());
        };
        if replacement.venue_order_id.is_some()
            || !matches!(replacement.status, OrderStatus::Recorded)
        {
            return Ok(RecoveryOutcome::local_only());
        }
        let market = markets
            .iter()
            .find(|market| market.market_id == replacement.intent.market_id)
            .with_context(|| {
                format!(
                    "failed to locate market {} for replacement of order {}",
                    replacement.intent.market_id, local_order.intent.id
                )
            })?;
        self.submit(market, replacement.clone()).await?;
        if matches!(replacement.intent.action, OrderIntentAction::Close) {
            reserve_pending_close(&self.storage, replacement)?;
        }
        Ok(RecoveryOutcome {
            remote_cancel_attempts: 0,
            local_state_fixes: 1,
            replacement_submissions: 1,
        })
    }
}

fn apply_trade_message(storage: &Storage, message: TradeMessage) -> Result<()> {
    let mut matched_orders = Vec::new();
    if let Some(taker_order_id) = message.taker_order_id.as_ref() {
        matched_orders.push(taker_order_id.clone());
    }
    matched_orders.extend(
        message
            .maker_orders
            .iter()
            .map(|order| order.order_id.clone()),
    );
    if matched_orders.is_empty() {
        storage.append_event(&Event::NotificationQueued(format!(
            "trade stream | trade_id={} | unmatched_local_order",
            message.id
        )))?;
        return Ok(());
    }

    let local_orders = storage.list_orders()?;
    let price_f64 = decimal_to_f64(&message.price)?;
    let qty_f64 = decimal_to_f64(&message.size)?;

    for local_order in local_orders {
        let Some(venue_order_id) = local_order.venue_order_id.as_ref() else {
            continue;
        };
        if matched_orders.iter().any(|id| id == venue_order_id) {
            let mut updated_order = local_order.clone();
            updated_order.filled_shares += qty_f64;
            updated_order.remaining_shares =
                (updated_order.intent.quantity_shares - updated_order.filled_shares).max(0.0);
            updated_order.status = if updated_order.remaining_shares <= 1e-9 {
                OrderStatus::Filled
            } else {
                OrderStatus::Sent
            };
            updated_order.last_update = Utc::now();
            storage.save_order(&updated_order)?;
            storage.append_event(&Event::OrderUpdated(updated_order.clone()))?;
            upsert_position_from_trade(storage, &local_order, qty_f64, price_f64)?;
            storage.append_event(&Event::NotificationQueued(format!(
                "trade stream | trade_id={} | order_id={} | market={} | qty={:.4} | price={:.4}",
                message.id, venue_order_id, local_order.intent.market_id, qty_f64, price_f64
            )))?;
        }
    }

    Ok(())
}

fn find_unsent_replacement<'a>(
    local_order: &ManagedOrder,
    local_orders: &'a [ManagedOrder],
) -> Option<&'a ManagedOrder> {
    local_orders.iter().find(|candidate| {
        candidate.replacement_for == Some(local_order.intent.id)
            && candidate.client_intent_id == local_order.client_intent_id
            && candidate.venue_order_id.is_none()
    })
}

fn sync_local_order(
    storage: &Storage,
    local_order: &ManagedOrder,
    status: OrderStatus,
    filled_shares: f64,
    remaining_shares: f64,
) -> Result<()> {
    let mut updated_order = local_order.clone();
    updated_order.status = status;
    updated_order.filled_shares = filled_shares;
    updated_order.remaining_shares = remaining_shares;
    updated_order.last_update = Utc::now();
    storage.save_order(&updated_order)?;
    storage.append_event(&Event::OrderUpdated(updated_order))?;
    Ok(())
}

fn finalize_local_cancellation(
    storage: &Storage,
    local_order: &ManagedOrder,
    release_pending_close: bool,
) -> Result<()> {
    if release_pending_close && matches!(local_order.intent.action, OrderIntentAction::Close) {
        release_pending_close_shares(
            storage,
            &local_order.intent.market_id,
            local_order.intent.side,
            local_order.remaining_shares,
        )?;
    }

    let mut cancelled = local_order.clone();
    cancelled.status = OrderStatus::Cancelled;
    cancelled.remaining_shares = 0.0;
    cancelled.last_update = Utc::now();
    storage.save_order(&cancelled)?;
    storage.append_event(&Event::OrderCancelled(
        local_order
            .venue_order_id
            .clone()
            .unwrap_or_else(|| local_order.intent.id.to_string()),
    ))?;
    Ok(())
}

fn upsert_position_from_trade(
    storage: &Storage,
    order: &ManagedOrder,
    filled_qty: f64,
    fill_price: f64,
) -> Result<()> {
    let mut positions = storage.list_positions()?;
    if matches!(order.intent.action, OrderIntentAction::Close) {
        if let Some(existing) = positions.iter_mut().find(|position| {
            position.market_id == order.intent.market_id
                && position.side == order.intent.side
                && position.exit_reason.is_none()
        }) {
            let remaining_qty = (existing.quantity_shares - filled_qty).max(0.0);
            existing.quantity_shares = remaining_qty;
            existing.pending_close_shares = (existing.pending_close_shares - filled_qty).max(0.0);
            existing.size_usd = remaining_qty * existing.average_price;
            existing.updated_at = Utc::now();
            if remaining_qty <= 1e-9 {
                existing.closed_at = Some(Utc::now());
                existing.exit_reason = Some("trade_fill".to_string());
            }
            storage.save_position(existing)?;
        }
        return Ok(());
    }

    if let Some(existing) = positions.iter_mut().find(|position| {
        position.market_id == order.intent.market_id
            && position.side == order.intent.side
            && position.exit_reason.is_none()
    }) {
        let combined_qty = existing.quantity_shares + filled_qty;
        let combined_notional = existing.size_usd + (filled_qty * fill_price);
        existing.average_price = if combined_qty > 0.0 {
            combined_notional / combined_qty
        } else {
            fill_price
        };
        existing.quantity_shares = combined_qty;
        existing.size_usd = combined_notional;
        existing.updated_at = Utc::now();
        storage.save_position(existing)?;
        return Ok(());
    }

    let position = Position {
        position_id: Uuid::new_v4(),
        market_id: order.intent.market_id.clone(),
        side: order.intent.side,
        quantity_shares: filled_qty,
        pending_close_shares: 0.0,
        average_price: fill_price,
        size_usd: filled_qty * fill_price,
        opened_at: Utc::now(),
        updated_at: Utc::now(),
        closed_at: None,
        exit_reason: None,
    };
    storage.save_position(&position)?;
    Ok(())
}

fn reserve_pending_close(storage: &Storage, order: &ManagedOrder) -> Result<()> {
    let mut positions = storage.list_positions()?;
    if let Some(position) = positions.iter_mut().find(|position| {
        position.market_id == order.intent.market_id
            && position.side == order.intent.side
            && position.exit_reason.is_none()
    }) {
        position.pending_close_shares += order.intent.quantity_shares;
        position.updated_at = Utc::now();
        storage.save_position(position)?;
    }
    Ok(())
}

fn release_pending_close_shares(
    storage: &Storage,
    market_id: &str,
    side: PositionSide,
    released_shares: f64,
) -> Result<()> {
    let mut positions = storage.list_positions()?;
    if let Some(position) = positions.iter_mut().find(|position| {
        position.market_id == market_id && position.side == side && position.exit_reason.is_none()
    }) {
        position.pending_close_shares = (position.pending_close_shares - released_shares).max(0.0);
        position.updated_at = Utc::now();
        storage.save_position(position)?;
    }
    Ok(())
}

fn decimal_to_f64(value: &Decimal) -> Result<f64> {
    value
        .to_string()
        .parse::<f64>()
        .context("failed to convert Decimal to f64")
}
