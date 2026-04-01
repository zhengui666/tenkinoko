use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WeatherMetric {
    HighTempC,
    LowTempC,
    PrecipitationMm,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ComparisonOp {
    Above,
    Below,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WeatherMarketKind {
    DailyHigh {
        threshold_c: f64,
    },
    DailyLow {
        threshold_c: f64,
    },
    Threshold {
        metric: WeatherMetric,
        op: ComparisonOp,
        threshold: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketSpec {
    pub city: String,
    pub station_id: Option<String>,
    pub target_date: NaiveDate,
    pub kind: WeatherMarketKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Market {
    pub market_id: String,
    pub condition_id: Option<String>,
    pub slug: String,
    pub question: String,
    pub yes_token_id: Option<String>,
    pub no_token_id: Option<String>,
    pub spec: MarketSpec,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub active: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastSample {
    pub source: String,
    pub issued_at: DateTime<Utc>,
    pub valid_for: DateTime<Utc>,
    pub temperature_c: Option<f64>,
    pub precipitation_mm: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastBundle {
    pub city: String,
    pub latitude: f64,
    pub longitude: f64,
    pub issued_at: DateTime<Utc>,
    pub samples: Vec<ForecastSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationSnapshot {
    pub city: String,
    pub station_id: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub temperature_c: Option<f64>,
    pub precipitation_mm: Option<f64>,
    pub raw_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderbookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderbookDelta {
    pub market_id: String,
    pub token_id: String,
    pub captured_at: DateTime<Utc>,
    pub bids: Vec<OrderbookLevel>,
    pub asks: Vec<OrderbookLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketRuntimeSnapshot {
    pub market_id: String,
    pub captured_at: DateTime<Utc>,
    pub yes_midpoint: Option<f64>,
    pub no_midpoint: Option<f64>,
    pub yes_spread: Option<f64>,
    pub no_spread: Option<f64>,
    pub yes_last_trade: Option<f64>,
    pub no_last_trade: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PriceHistoryPoint {
    pub timestamp: DateTime<Utc>,
    pub price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PriceHistorySeries {
    pub market_id: String,
    pub token_id: String,
    pub captured_at: DateTime<Utc>,
    pub points: Vec<PriceHistoryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PosteriorEstimate {
    pub market_id: String,
    pub probability_yes: f64,
    pub fair_value: f64,
    pub confidence: f64,
    pub sample_size: usize,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmInsight {
    pub market_id: String,
    pub summary: String,
    pub caution_flags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TelegramDeliveryStatus {
    Pending,
    Sent,
    Failed,
    DeadLetter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelegramOutboxMessage {
    pub id: Uuid,
    pub body: String,
    pub status: TelegramDeliveryStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandInboxMessage {
    pub id: Uuid,
    pub source: String,
    pub command: String,
    pub received_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobRecord {
    pub id: Uuid,
    pub kind: String,
    pub payload: String,
    pub status: JobStatus,
    pub attempts: u32,
    pub not_before: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SchedulerCheckpoint {
    pub name: String,
    pub last_run_at: DateTime<Utc>,
    pub next_run_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DateMapping {
    pub id: Uuid,
    pub real_date: NaiveDate,
    pub fake_date: NaiveDate,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalSide {
    BuyYes,
    BuyNo,
    Hold,
    Exit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TradeSignal {
    pub market_id: String,
    pub generated_at: DateTime<Utc>,
    pub side: SignalSide,
    pub edge_bps: i64,
    pub max_size_usd: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskState {
    Normal,
    Cautious,
    ReduceOnly,
    HaltOpen,
    EmergencyFlat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RiskDecision {
    pub market_id: String,
    pub state: RiskState,
    pub approved: bool,
    pub capped_size_usd: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderStatus {
    New,
    Recorded,
    PendingCancel,
    PendingReplace,
    Sent,
    Filled,
    Cancelled,
    Rejected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PositionSide {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderIntentAction {
    Open,
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderIntent {
    pub id: Uuid,
    pub market_id: String,
    pub created_at: DateTime<Utc>,
    pub side: PositionSide,
    pub action: OrderIntentAction,
    pub quantity_shares: f64,
    pub limit_price: f64,
    pub size_usd: f64,
    pub maker_only: bool,
    pub tif: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManagedOrder {
    pub client_intent_id: Uuid,
    pub intent: OrderIntent,
    pub status: OrderStatus,
    pub filled_shares: f64,
    pub remaining_shares: f64,
    pub replacement_for: Option<Uuid>,
    pub venue_order_id: Option<String>,
    pub last_update: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub position_id: Uuid,
    pub market_id: String,
    pub side: PositionSide,
    pub quantity_shares: f64,
    pub pending_close_shares: f64,
    pub average_price: f64,
    pub size_usd: f64,
    pub opened_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub exit_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Event {
    MarketDiscovered(Market),
    MarketRuntimeCaptured(MarketRuntimeSnapshot),
    OrderbookCaptured(OrderbookDelta),
    PriceHistoryCaptured(PriceHistorySeries),
    ForecastIngested(ForecastBundle),
    ObservationIngested(ObservationSnapshot),
    PosteriorComputed(PosteriorEstimate),
    LlmInsightComputed(LlmInsight),
    SignalGenerated(TradeSignal),
    RiskEvaluated(RiskDecision),
    OrderRecorded(ManagedOrder),
    OrderUpdated(ManagedOrder),
    OrderCancelled(String),
    TelegramQueued(TelegramOutboxMessage),
    TelegramDelivered(Uuid),
    CommandReceived(CommandInboxMessage),
    JobQueued(JobRecord),
    NotificationQueued(String),
}
