use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub storage_path: PathBuf,
    pub total_equity_usd: f64,
    pub max_position_pct: f64,
    pub city_filters: Vec<String>,
    pub market_limit: usize,
    pub cycle_interval_secs: u64,
    pub min_edge_bps: i64,
    pub fees_bps: i64,
    pub slippage_bps: i64,
    pub cluster_max_pct: f64,
    pub daily_loss_limit_usd: f64,
    pub market_anomaly_spread_bps: i64,
    pub reprice_threshold_bps: i64,
    pub prereso_exit_hours: i64,
    pub observation_mismatch_c: f64,
    pub source_divergence_c: f64,
    pub noaa_base_url: String,
    pub noaa_enabled: bool,
    pub polymarket_gamma_url: String,
    pub execution: PolymarketExecutionConfig,
    pub openmeteo_base_url: String,
    pub openmeteo_historical_base_url: String,
    pub openmeteo_geocoding_url: String,
    pub openai_base_url: String,
    pub openai_model: String,
    pub openai_api_key: Option<String>,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub telegram_readonly_chat_ids: Vec<String>,
    pub telegram_admin_chat_ids: Vec<String>,
    pub telegram_max_delivery_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymarketExecutionConfig {
    pub live_trading: bool,
    pub clob_url: String,
    pub private_key: Option<String>,
    pub funder_address: Option<String>,
    pub signature_type: u8,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            storage_path: PathBuf::from(var_or("TENKINOKO_STORAGE_PATH", "./data/rocksdb")),
            total_equity_usd: parse_var("TENKINOKO_TOTAL_EQUITY_USD", 10_000.0)?,
            max_position_pct: parse_var("TENKINOKO_MAX_POSITION_PCT", 0.02)?,
            city_filters: csv_var("TENKINOKO_CITY_FILTERS"),
            market_limit: parse_var("TENKINOKO_MARKET_LIMIT", 50usize)?,
            cycle_interval_secs: parse_var("TENKINOKO_CYCLE_INTERVAL_SECS", 300u64)?,
            min_edge_bps: parse_var("TENKINOKO_MIN_EDGE_BPS", 300i64)?,
            fees_bps: parse_var("TENKINOKO_FEES_BPS", 100i64)?,
            slippage_bps: parse_var("TENKINOKO_SLIPPAGE_BPS", 40i64)?,
            cluster_max_pct: parse_var("TENKINOKO_CLUSTER_MAX_POSITION_PCT", 0.05)?,
            daily_loss_limit_usd: parse_var("TENKINOKO_DAILY_LOSS_LIMIT_USD", 250.0f64)?,
            market_anomaly_spread_bps: parse_var("TENKINOKO_MARKET_ANOMALY_SPREAD_BPS", 600i64)?,
            reprice_threshold_bps: parse_var("TENKINOKO_REPRICE_THRESHOLD_BPS", 75i64)?,
            prereso_exit_hours: parse_var("TENKINOKO_PRERESOLUTION_EXIT_HOURS", 6i64)?,
            observation_mismatch_c: parse_var("TENKINOKO_OBSERVATION_MISMATCH_C", 8.0f64)?,
            source_divergence_c: parse_var("TENKINOKO_SOURCE_DIVERGENCE_C", 4.0f64)?,
            noaa_base_url: var_or("TENKINOKO_NOAA_BASE_URL", "https://api.weather.gov"),
            noaa_enabled: bool_var("TENKINOKO_ENABLE_NOAA", true),
            polymarket_gamma_url: var_or(
                "TENKINOKO_POLYMARKET_GAMMA_URL",
                "https://gamma-api.polymarket.com",
            ),
            execution: PolymarketExecutionConfig {
                live_trading: bool_var("TENKINOKO_POLYMARKET_LIVE_TRADING", false),
                clob_url: var_or(
                    "TENKINOKO_POLYMARKET_CLOB_URL",
                    "https://clob.polymarket.com",
                ),
                private_key: env::var("POLYMARKET_PRIVATE_KEY")
                    .ok()
                    .filter(|v| !v.is_empty()),
                funder_address: env::var("POLYMARKET_FUNDER_ADDRESS")
                    .ok()
                    .filter(|v| !v.is_empty()),
                signature_type: parse_var("POLYMARKET_SIGNATURE_TYPE", 0u8)?,
            },
            openmeteo_base_url: var_or(
                "TENKINOKO_OPENMETEO_BASE_URL",
                "https://api.open-meteo.com",
            ),
            openmeteo_historical_base_url: var_or(
                "TENKINOKO_OPENMETEO_HISTORICAL_BASE_URL",
                "https://historical-forecast-api.open-meteo.com",
            ),
            openmeteo_geocoding_url: var_or(
                "TENKINOKO_OPENMETEO_GEOCODING_URL",
                "https://geocoding-api.open-meteo.com",
            ),
            openai_base_url: var_or("TENKINOKO_OPENAI_BASE_URL", "https://api.openai.com/v1"),
            openai_model: var_or("TENKINOKO_OPENAI_MODEL", "gpt-4o-mini"),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|v| !v.is_empty()),
            telegram_bot_token: env::var("TELEGRAM_BOT_TOKEN")
                .ok()
                .filter(|v| !v.is_empty()),
            telegram_chat_id: env::var("TELEGRAM_CHAT_ID").ok().filter(|v| !v.is_empty()),
            telegram_readonly_chat_ids: csv_var("TENKINOKO_TELEGRAM_READONLY_CHAT_IDS"),
            telegram_admin_chat_ids: csv_var("TENKINOKO_TELEGRAM_ADMIN_CHAT_IDS"),
            telegram_max_delivery_attempts: parse_var(
                "TENKINOKO_TELEGRAM_MAX_DELIVERY_ATTEMPTS",
                5u32,
            )?,
        })
    }

    pub fn per_market_exposure_limit_usd(&self) -> f64 {
        self.total_equity_usd * self.max_position_pct
    }
}

impl PolymarketExecutionConfig {
    pub fn live_enabled(&self) -> bool {
        self.live_trading
    }
}

fn var_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn csv_var(key: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn bool_var(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn parse_var<T>(key: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(value) => value
            .parse()
            .map_err(|error| anyhow::anyhow!("failed to parse env var {key}={value}: {error}")),
        Err(_) => Ok(default),
    }
}
