use anyhow::{Context, Result};
use chrono::Utc;
use domain_core::{
    CommandInboxMessage, DateMapping, Event, ForecastBundle, JobRecord, LlmInsight, ManagedOrder,
    Market, MarketRuntimeSnapshot, ObservationSnapshot, OrderbookDelta, Position,
    PosteriorEstimate, PriceHistorySeries, RiskDecision, RiskState, SchedulerCheckpoint,
    TelegramDeliveryStatus, TelegramOutboxMessage, TradeSignal,
};
use rocksdb::{ColumnFamilyDescriptor, DB, IteratorMode, Options};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

pub const CF_MARKET_META: &str = "market_meta";
pub const CF_MARKET_RUNTIME: &str = "market_runtime";
pub const CF_ORDERBOOK_DELTA: &str = "orderbook_delta";
pub const CF_PRICE_HISTORY: &str = "price_history";
pub const CF_WEATHER_FORECAST_RAW: &str = "weather_forecast_raw";
pub const CF_WEATHER_OBS_RAW: &str = "weather_obs_raw";
pub const CF_FEATURES: &str = "features";
pub const CF_LLM_INSIGHTS: &str = "llm_insights";
pub const CF_SIGNALS: &str = "signals";
pub const CF_ORDERS: &str = "orders";
pub const CF_POSITIONS: &str = "positions";
pub const CF_RISK_STATE: &str = "risk_state";
pub const CF_TELEGRAM_OUTBOX: &str = "telegram_outbox";
pub const CF_COMMAND_INBOX: &str = "command_inbox";
pub const CF_JOB_QUEUE: &str = "job_queue";
pub const CF_SCHEDULER_CHECKPOINT: &str = "scheduler_checkpoint";
pub const CF_DATE_MAP: &str = "date_map";
pub const CF_EVENT_LOG: &str = "event_log";

const ALL_CFS: [&str; 18] = [
    CF_MARKET_META,
    CF_MARKET_RUNTIME,
    CF_ORDERBOOK_DELTA,
    CF_PRICE_HISTORY,
    CF_WEATHER_FORECAST_RAW,
    CF_WEATHER_OBS_RAW,
    CF_FEATURES,
    CF_LLM_INSIGHTS,
    CF_SIGNALS,
    CF_ORDERS,
    CF_POSITIONS,
    CF_RISK_STATE,
    CF_TELEGRAM_OUTBOX,
    CF_COMMAND_INBOX,
    CF_JOB_QUEUE,
    CF_SCHEDULER_CHECKPOINT,
    CF_DATE_MAP,
    CF_EVENT_LOG,
];

#[derive(Clone)]
pub struct Storage {
    db: Arc<DB>,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        let descriptors = ALL_CFS
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect::<Vec<_>>();

        let db = DB::open_cf_descriptors(&db_opts, path, descriptors)?;
        Ok(Self { db: Arc::new(db) })
    }

    pub fn save_market(&self, market: &Market) -> Result<()> {
        let key = format!("market_meta#{}", market.market_id);
        self.put_json(CF_MARKET_META, &key, market)
    }

    pub fn save_market_runtime(&self, runtime: &MarketRuntimeSnapshot) -> Result<()> {
        let key = format!(
            "market_runtime#{}#{}",
            runtime.market_id,
            runtime.captured_at.timestamp_millis()
        );
        self.put_json(CF_MARKET_RUNTIME, &key, runtime)
    }

    pub fn save_orderbook_delta(&self, delta: &OrderbookDelta) -> Result<()> {
        let key = format!(
            "orderbook_delta#{}#{}#{}",
            delta.market_id,
            delta.token_id,
            delta.captured_at.timestamp_millis()
        );
        self.put_json(CF_ORDERBOOK_DELTA, &key, delta)
    }

    pub fn save_price_history(&self, history: &PriceHistorySeries) -> Result<()> {
        let key = format!(
            "price_history#{}#{}#{}",
            history.market_id,
            history.token_id,
            history.captured_at.timestamp_millis()
        );
        self.put_json(CF_PRICE_HISTORY, &key, history)
    }

    pub fn save_forecast(&self, market_id: &str, forecast: &ForecastBundle) -> Result<()> {
        let key = format!("forecast#{market_id}#{}", forecast.issued_at.timestamp());
        self.put_json(CF_WEATHER_FORECAST_RAW, &key, forecast)
    }

    pub fn save_observation(
        &self,
        market_id: &str,
        observation: &ObservationSnapshot,
    ) -> Result<()> {
        let key = format!(
            "observation#{market_id}#{}",
            observation.observed_at.timestamp()
        );
        self.put_json(CF_WEATHER_OBS_RAW, &key, observation)
    }

    pub fn save_posterior(&self, posterior: &PosteriorEstimate) -> Result<()> {
        let key = format!(
            "posterior#{}#{}",
            posterior.market_id,
            Utc::now().timestamp()
        );
        self.put_json(CF_FEATURES, &key, posterior)
    }

    pub fn save_signal(&self, signal: &TradeSignal) -> Result<()> {
        let key = format!(
            "signal#{}#{}",
            signal.market_id,
            signal.generated_at.timestamp()
        );
        self.put_json(CF_SIGNALS, &key, signal)
    }

    pub fn save_llm_insight(&self, insight: &LlmInsight) -> Result<()> {
        let key = format!("llm#{}#{}", insight.market_id, Utc::now().timestamp());
        self.put_json(CF_LLM_INSIGHTS, &key, insight)
    }

    pub fn save_order(&self, order: &ManagedOrder) -> Result<()> {
        let key = format!("order#{}#{}", order.intent.market_id, order.intent.id);
        self.put_json(CF_ORDERS, &key, order)
    }

    pub fn save_position(&self, position: &Position) -> Result<()> {
        let key = format!("position#{}", position.position_id);
        self.put_json(CF_POSITIONS, &key, position)
    }

    pub fn set_risk_state(&self, state: &RiskState) -> Result<()> {
        self.put_json(CF_RISK_STATE, "risk_state#current", state)
    }

    pub fn get_risk_state(&self) -> Result<Option<RiskState>> {
        self.get_json(CF_RISK_STATE, "risk_state#current")
    }

    pub fn save_risk_decision(&self, decision: &RiskDecision) -> Result<()> {
        let key = format!("risk#{}#{}", decision.market_id, Utc::now().timestamp());
        self.put_json(CF_RISK_STATE, &key, decision)
    }

    pub fn append_event(&self, event: &Event) -> Result<()> {
        let key = format!("event#{}#{}", Utc::now().timestamp_millis(), Uuid::new_v4());
        self.put_json(CF_EVENT_LOG, &key, event)
    }

    pub fn save_telegram_message(&self, message: &TelegramOutboxMessage) -> Result<()> {
        let key = format!(
            "outbox#{:?}#{}#{}",
            message.status,
            message.created_at.timestamp_millis(),
            message.id
        );
        self.put_json(CF_TELEGRAM_OUTBOX, &key, message)
    }

    pub fn enqueue_telegram_message(&self, body: &str) -> Result<TelegramOutboxMessage> {
        let now = Utc::now();
        let message = TelegramOutboxMessage {
            id: Uuid::new_v4(),
            body: body.to_string(),
            status: TelegramDeliveryStatus::Pending,
            attempts: 0,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        self.save_telegram_message(&message)?;
        self.append_event(&Event::TelegramQueued(message.clone()))?;
        Ok(message)
    }

    pub fn save_command(&self, command: &CommandInboxMessage) -> Result<()> {
        let key = format!(
            "command#{}#{}",
            command.received_at.timestamp_millis(),
            command.id
        );
        self.put_json(CF_COMMAND_INBOX, &key, command)
    }

    pub fn save_job(&self, job: &JobRecord) -> Result<()> {
        let key = format!(
            "job#{}#{}#{}",
            job.not_before.timestamp_millis(),
            job.kind,
            job.id
        );
        self.put_json(CF_JOB_QUEUE, &key, job)
    }

    pub fn save_scheduler_checkpoint(&self, checkpoint: &SchedulerCheckpoint) -> Result<()> {
        let key = format!("scheduler#{}", checkpoint.name);
        self.put_json(CF_SCHEDULER_CHECKPOINT, &key, checkpoint)
    }

    pub fn save_date_mapping(&self, mapping: &DateMapping) -> Result<()> {
        let key = format!("date-map#{}#{}", mapping.real_date, mapping.id);
        self.put_json(CF_DATE_MAP, &key, mapping)
    }

    pub fn list_positions(&self) -> Result<Vec<Position>> {
        self.list_json(CF_POSITIONS)
    }

    pub fn list_orders(&self) -> Result<Vec<ManagedOrder>> {
        self.list_json(CF_ORDERS)
    }

    pub fn list_markets(&self) -> Result<Vec<Market>> {
        self.list_json(CF_MARKET_META)
    }

    pub fn list_market_runtime(&self) -> Result<Vec<MarketRuntimeSnapshot>> {
        self.list_json(CF_MARKET_RUNTIME)
    }

    pub fn list_orderbook_deltas(&self) -> Result<Vec<OrderbookDelta>> {
        self.list_json(CF_ORDERBOOK_DELTA)
    }

    pub fn list_price_history(&self) -> Result<Vec<PriceHistorySeries>> {
        self.list_json(CF_PRICE_HISTORY)
    }

    pub fn list_forecasts(&self) -> Result<Vec<ForecastBundle>> {
        self.list_json(CF_WEATHER_FORECAST_RAW)
    }

    pub fn list_observations(&self) -> Result<Vec<ObservationSnapshot>> {
        self.list_json(CF_WEATHER_OBS_RAW)
    }

    pub fn list_llm_insights(&self) -> Result<Vec<LlmInsight>> {
        self.list_json(CF_LLM_INSIGHTS)
    }

    pub fn list_signals(&self) -> Result<Vec<TradeSignal>> {
        self.list_json(CF_SIGNALS)
    }

    pub fn list_posteriors(&self) -> Result<Vec<PosteriorEstimate>> {
        self.list_json(CF_FEATURES)
    }

    pub fn list_telegram_messages(&self) -> Result<Vec<TelegramOutboxMessage>> {
        self.list_json(CF_TELEGRAM_OUTBOX)
    }

    pub fn list_commands(&self) -> Result<Vec<CommandInboxMessage>> {
        self.list_json(CF_COMMAND_INBOX)
    }

    pub fn list_jobs(&self) -> Result<Vec<JobRecord>> {
        self.list_json(CF_JOB_QUEUE)
    }

    pub fn list_scheduler_checkpoints(&self) -> Result<Vec<SchedulerCheckpoint>> {
        self.list_json(CF_SCHEDULER_CHECKPOINT)
    }

    pub fn list_date_mappings(&self) -> Result<Vec<DateMapping>> {
        self.list_json(CF_DATE_MAP)
    }

    fn put_json<T: Serialize>(&self, cf_name: &str, key: &str, value: &T) -> Result<()> {
        let cf = self
            .db
            .cf_handle(cf_name)
            .with_context(|| format!("missing column family {cf_name}"))?;
        let bytes = serde_json::to_vec(value)?;
        self.db.put_cf(&cf, key.as_bytes(), bytes)?;
        Ok(())
    }

    fn get_json<T: DeserializeOwned>(&self, cf_name: &str, key: &str) -> Result<Option<T>> {
        let cf = self
            .db
            .cf_handle(cf_name)
            .with_context(|| format!("missing column family {cf_name}"))?;
        let Some(bytes) = self.db.get_cf(&cf, key.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    fn list_json<T: DeserializeOwned>(&self, cf_name: &str) -> Result<Vec<T>> {
        let cf = self
            .db
            .cf_handle(cf_name)
            .with_context(|| format!("missing column family {cf_name}"))?;
        self.db
            .iterator_cf(&cf, IteratorMode::Start)
            .map(|entry| {
                let (_, value) = entry?;
                serde_json::from_slice(&value).context("failed to deserialize RocksDB value")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Storage;
    use chrono::Utc;
    use domain_core::{DateMapping, LlmInsight, RiskState, TelegramDeliveryStatus};
    use std::time::{SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

    #[test]
    fn risk_state_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tenkinoko-storage-test-{unique}"));
        let storage = Storage::open(&path).expect("open storage");
        storage
            .set_risk_state(&RiskState::Cautious)
            .expect("set state");
        let state = storage
            .get_risk_state()
            .expect("get state")
            .expect("present");
        assert_eq!(state, RiskState::Cautious);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn llm_insight_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tenkinoko-storage-llm-test-{unique}"));
        let storage = Storage::open(&path).expect("open storage");
        let insight = LlmInsight {
            market_id: "market-1".to_string(),
            summary: "sources diverged".to_string(),
            caution_flags: vec!["spread_wide".to_string(), "forecast_divergence".to_string()],
        };
        storage
            .save_llm_insight(&insight)
            .expect("save llm insight");
        let insights = storage.list_llm_insights().expect("list llm insights");
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0], insight);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn telegram_outbox_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tenkinoko-storage-outbox-test-{unique}"));
        let storage = Storage::open(&path).expect("open storage");
        let message = storage.enqueue_telegram_message("hello").expect("enqueue");
        let entries = storage.list_telegram_messages().expect("list outbox");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, "hello");
        assert_eq!(entries[0].status, TelegramDeliveryStatus::Pending);
        assert_eq!(entries[0].id, message.id);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn date_mapping_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tenkinoko-storage-date-map-test-{unique}"));
        let storage = Storage::open(&path).expect("open storage");
        let mapping = DateMapping {
            id: Uuid::new_v4(),
            real_date: chrono::NaiveDate::from_ymd_opt(2025, 2, 28).expect("date"),
            fake_date: chrono::NaiveDate::from_ymd_opt(2037, 2, 28).expect("date"),
            created_at: Utc::now(),
        };
        storage.save_date_mapping(&mapping).expect("save date map");
        let entries = storage.list_date_mappings().expect("list date maps");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], mapping);
        let _ = std::fs::remove_dir_all(path);
    }
}
