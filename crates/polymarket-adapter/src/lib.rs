use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use domain_core::{
    ComparisonOp, Market, MarketRuntimeSnapshot, MarketSpec, OrderbookDelta, OrderbookLevel,
    PriceHistoryPoint, PriceHistorySeries, WeatherMarketKind, WeatherMetric,
};
use futures_util::StreamExt;
use polymarket_client_sdk::clob::types::request::{
    LastTradePriceRequest, MidpointRequest, OrderBookSummaryRequest, PriceHistoryRequest,
    SpreadRequest,
};
use polymarket_client_sdk::clob::types::{Interval, Side, TimeRange};
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::clob::ws::types::response::{BookUpdate, LastTradePrice, PriceChange};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::{Decimal, U256};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::str::FromStr;
use storage_rocksdb::Storage;
use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct PolymarketClient {
    base_url: String,
    clob_url: String,
    http: reqwest::Client,
}

impl PolymarketClient {
    pub fn new(base_url: impl Into<String>, clob_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            clob_url: clob_url.into(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn fetch_weather_markets(&self, limit: usize) -> Result<Vec<Market>> {
        let url = format!(
            "{}/markets?closed=false&limit={limit}",
            self.base_url.trim_end_matches('/')
        );
        let payload = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to request Polymarket Gamma markets")?
            .error_for_status()
            .context("Polymarket Gamma returned non-success status")?
            .json::<Vec<Value>>()
            .await
            .context("failed to decode Polymarket market payload")?;

        payload
            .into_iter()
            .filter_map(|value| map_market(value).transpose())
            .collect()
    }

    pub async fn fetch_market_runtime(
        &self,
        market: &Market,
    ) -> Result<(MarketRuntimeSnapshot, Vec<OrderbookDelta>)> {
        let client = Client::new(&self.clob_url, Config::default())
            .context("failed to create public CLOB client")?;
        let yes = fetch_token_runtime(&client, market, market.yes_token_id.as_deref()).await?;
        let no = fetch_token_runtime(&client, market, market.no_token_id.as_deref()).await?;

        let snapshot = MarketRuntimeSnapshot {
            market_id: market.market_id.clone(),
            captured_at: Utc::now(),
            yes_midpoint: yes.as_ref().and_then(|runtime| runtime.midpoint),
            no_midpoint: no.as_ref().and_then(|runtime| runtime.midpoint),
            yes_spread: yes.as_ref().and_then(|runtime| runtime.spread),
            no_spread: no.as_ref().and_then(|runtime| runtime.spread),
            yes_last_trade: yes.as_ref().and_then(|runtime| runtime.last_trade),
            no_last_trade: no.as_ref().and_then(|runtime| runtime.last_trade),
        };

        let deltas = yes
            .into_iter()
            .chain(no)
            .map(|runtime| runtime.orderbook)
            .collect::<Vec<_>>();

        Ok((snapshot, deltas))
    }

    pub async fn fetch_price_history(&self, market: &Market) -> Result<Vec<PriceHistorySeries>> {
        let client = Client::new(&self.clob_url, Config::default())
            .context("failed to create public CLOB client")?;
        let mut series = Vec::new();
        for token in [
            market.yes_token_id.as_deref(),
            market.no_token_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let token_id = U256::from_str(token)
                .with_context(|| format!("failed to parse token id {token}"))?;
            let request = PriceHistoryRequest::builder()
                .market(token_id)
                .time_range(TimeRange::Interval {
                    interval: Interval::OneDay,
                })
                .fidelity(24)
                .build();
            let response = client
                .price_history(&request)
                .await
                .context("failed to fetch Polymarket price history")?;
            series.push(PriceHistorySeries {
                market_id: market.market_id.clone(),
                token_id: token.to_string(),
                captured_at: Utc::now(),
                points: response
                    .history
                    .into_iter()
                    .map(|point| PriceHistoryPoint {
                        timestamp: Utc
                            .timestamp_opt(point.t, 0)
                            .single()
                            .unwrap_or_else(Utc::now),
                        price: decimal_to_f64(&point.p).unwrap_or_default(),
                    })
                    .collect(),
            });
        }
        Ok(series)
    }

    pub async fn spawn_market_stream_sync(&self, storage: Storage) -> Result<Vec<JoinHandle<()>>> {
        let asset_ids = storage
            .list_markets()?
            .into_iter()
            .flat_map(|market| [market.yes_token_id, market.no_token_id])
            .flatten()
            .filter_map(|token| U256::from_str(&token).ok())
            .collect::<Vec<_>>();
        if asset_ids.is_empty() {
            return Ok(Vec::new());
        }

        let client = WsClient::new(&self.clob_url, Default::default())
            .context("failed to create Polymarket public websocket client")?;

        let books_client = client.clone();
        let books_storage = storage.clone();
        let books_asset_ids = asset_ids.clone();
        let books = tokio::spawn(async move {
            if let Err(error) =
                run_book_stream_loop(books_storage, books_client, books_asset_ids).await
            {
                tracing::warn!(error = %error, "public book websocket sync loop exited");
            }
        });

        let prices_client = client.clone();
        let prices_storage = storage.clone();
        let prices_asset_ids = asset_ids.clone();
        let prices = tokio::spawn(async move {
            if let Err(error) =
                run_price_stream_loop(prices_storage, prices_client, prices_asset_ids).await
            {
                tracing::warn!(error = %error, "public price websocket sync loop exited");
            }
        });

        let trades_storage = storage;
        let trades = tokio::spawn(async move {
            if let Err(error) = run_last_trade_stream_loop(trades_storage, client, asset_ids).await
            {
                tracing::warn!(error = %error, "public last-trade websocket sync loop exited");
            }
        });

        Ok(vec![books, prices, trades])
    }
}

struct TokenRuntime {
    midpoint: Option<f64>,
    spread: Option<f64>,
    last_trade: Option<f64>,
    orderbook: OrderbookDelta,
}

async fn fetch_token_runtime(
    client: &Client,
    market: &Market,
    token: Option<&str>,
) -> Result<Option<TokenRuntime>> {
    let Some(token) = token else {
        return Ok(None);
    };
    let token_id =
        U256::from_str(token).with_context(|| format!("failed to parse token id {token}"))?;
    let midpoint = client
        .midpoint(&MidpointRequest::builder().token_id(token_id).build())
        .await
        .ok()
        .and_then(|response| decimal_to_f64(&response.mid).ok());
    let spread = client
        .spread(
            &SpreadRequest::builder()
                .token_id(token_id)
                .side(Side::Buy)
                .build(),
        )
        .await
        .ok()
        .and_then(|response| decimal_to_f64(&response.spread).ok());
    let last_trade = client
        .last_trade_price(&LastTradePriceRequest::builder().token_id(token_id).build())
        .await
        .ok()
        .and_then(|response| decimal_to_f64(&response.price).ok());
    let book = client
        .order_book(
            &OrderBookSummaryRequest::builder()
                .token_id(token_id)
                .side(Side::Buy)
                .build(),
        )
        .await
        .context("failed to fetch Polymarket order book")?;

    Ok(Some(TokenRuntime {
        midpoint,
        spread,
        last_trade,
        orderbook: OrderbookDelta {
            market_id: market.market_id.clone(),
            token_id: token.to_string(),
            captured_at: Utc::now(),
            bids: book
                .bids
                .into_iter()
                .map(|level| OrderbookLevel {
                    price: decimal_to_f64(&level.price).unwrap_or_default(),
                    size: decimal_to_f64(&level.size).unwrap_or_default(),
                })
                .collect(),
            asks: book
                .asks
                .into_iter()
                .map(|level| OrderbookLevel {
                    price: decimal_to_f64(&level.price).unwrap_or_default(),
                    size: decimal_to_f64(&level.size).unwrap_or_default(),
                })
                .collect(),
        },
    }))
}

fn decimal_to_f64(value: &Decimal) -> Result<f64> {
    value
        .to_string()
        .parse::<f64>()
        .context("failed to convert Decimal to f64")
}

async fn run_book_stream_loop(
    storage: Storage,
    client: WsClient,
    asset_ids: Vec<U256>,
) -> Result<()> {
    let mut stream = Box::pin(
        client
            .subscribe_orderbook(asset_ids)
            .context("failed to subscribe public orderbook stream")?,
    );
    while let Some(item) = stream.next().await {
        if let Ok(message) = item {
            persist_book_update(&storage, &message)?;
        }
    }
    Ok(())
}

async fn run_price_stream_loop(
    storage: Storage,
    client: WsClient,
    asset_ids: Vec<U256>,
) -> Result<()> {
    let mut stream = Box::pin(
        client
            .subscribe_prices(asset_ids)
            .context("failed to subscribe public price stream")?,
    );
    while let Some(item) = stream.next().await {
        if let Ok(message) = item {
            persist_price_change(&storage, &message)?;
        }
    }
    Ok(())
}

async fn run_last_trade_stream_loop(
    storage: Storage,
    client: WsClient,
    asset_ids: Vec<U256>,
) -> Result<()> {
    let mut stream = Box::pin(
        client
            .subscribe_last_trade_price(asset_ids)
            .context("failed to subscribe public last-trade stream")?,
    );
    while let Some(item) = stream.next().await {
        if let Ok(message) = item {
            persist_last_trade(&storage, &message)?;
        }
    }
    Ok(())
}

fn persist_book_update(storage: &Storage, update: &BookUpdate) -> Result<()> {
    let market_id = update.market.to_string();
    let token_id = update.asset_id.to_string();
    let delta = OrderbookDelta {
        market_id: market_id.clone(),
        token_id,
        captured_at: Utc
            .timestamp_millis_opt(update.timestamp)
            .single()
            .unwrap_or_else(Utc::now),
        bids: update
            .bids
            .iter()
            .map(|level| OrderbookLevel {
                price: decimal_to_f64(&level.price).unwrap_or_default(),
                size: decimal_to_f64(&level.size).unwrap_or_default(),
            })
            .collect(),
        asks: update
            .asks
            .iter()
            .map(|level| OrderbookLevel {
                price: decimal_to_f64(&level.price).unwrap_or_default(),
                size: decimal_to_f64(&level.size).unwrap_or_default(),
            })
            .collect(),
    };
    storage.save_orderbook_delta(&delta)?;
    storage.append_event(&domain_core::Event::OrderbookCaptured(delta))?;
    Ok(())
}

fn persist_price_change(storage: &Storage, update: &PriceChange) -> Result<()> {
    let market_id = update.market.to_string();
    let captured_at = Utc
        .timestamp_millis_opt(update.timestamp)
        .single()
        .unwrap_or_else(Utc::now);
    let mut runtime = storage
        .list_market_runtime()?
        .into_iter()
        .filter(|entry| entry.market_id == market_id)
        .max_by_key(|entry| entry.captured_at)
        .unwrap_or(MarketRuntimeSnapshot {
            market_id: market_id.clone(),
            captured_at,
            yes_midpoint: None,
            no_midpoint: None,
            yes_spread: None,
            no_spread: None,
            yes_last_trade: None,
            no_last_trade: None,
        });
    runtime.captured_at = captured_at;
    for change in &update.price_changes {
        match token_side_for_asset(storage, &market_id, &change.asset_id.to_string()) {
            Some(TokenSide::Yes) => {
                runtime.yes_midpoint = decimal_to_f64(&change.price).ok();
                runtime.yes_spread = match (
                    change
                        .best_bid
                        .as_ref()
                        .and_then(|bid| decimal_to_f64(bid).ok()),
                    change
                        .best_ask
                        .as_ref()
                        .and_then(|ask| decimal_to_f64(ask).ok()),
                ) {
                    (Some(bid), Some(ask)) => Some((ask - bid).max(0.0)),
                    _ => runtime.yes_spread,
                };
            }
            Some(TokenSide::No) => {
                runtime.no_midpoint = decimal_to_f64(&change.price).ok();
                runtime.no_spread = match (
                    change
                        .best_bid
                        .as_ref()
                        .and_then(|bid| decimal_to_f64(bid).ok()),
                    change
                        .best_ask
                        .as_ref()
                        .and_then(|ask| decimal_to_f64(ask).ok()),
                ) {
                    (Some(bid), Some(ask)) => Some((ask - bid).max(0.0)),
                    _ => runtime.no_spread,
                };
            }
            None => {}
        }
    }
    storage.save_market_runtime(&runtime)?;
    storage.append_event(&domain_core::Event::MarketRuntimeCaptured(runtime))?;
    Ok(())
}

fn persist_last_trade(storage: &Storage, update: &LastTradePrice) -> Result<()> {
    let market_id = update.market.to_string();
    let captured_at = Utc
        .timestamp_millis_opt(update.timestamp)
        .single()
        .unwrap_or_else(Utc::now);
    let mut runtime = storage
        .list_market_runtime()?
        .into_iter()
        .filter(|entry| entry.market_id == market_id)
        .max_by_key(|entry| entry.captured_at)
        .unwrap_or(MarketRuntimeSnapshot {
            market_id: market_id.clone(),
            captured_at,
            yes_midpoint: None,
            no_midpoint: None,
            yes_spread: None,
            no_spread: None,
            yes_last_trade: None,
            no_last_trade: None,
        });
    runtime.captured_at = captured_at;
    match token_side_for_asset(storage, &market_id, &update.asset_id.to_string()) {
        Some(TokenSide::Yes) => runtime.yes_last_trade = decimal_to_f64(&update.price).ok(),
        Some(TokenSide::No) => runtime.no_last_trade = decimal_to_f64(&update.price).ok(),
        None => {}
    }
    storage.save_market_runtime(&runtime)?;
    storage.append_event(&domain_core::Event::MarketRuntimeCaptured(runtime))?;
    Ok(())
}

#[derive(Clone, Copy)]
enum TokenSide {
    Yes,
    No,
}

fn token_side_for_asset(storage: &Storage, market_id: &str, asset_id: &str) -> Option<TokenSide> {
    storage
        .list_markets()
        .ok()?
        .into_iter()
        .find(|market| market.market_id == market_id)
        .and_then(|market| {
            if market.yes_token_id.as_deref() == Some(asset_id) {
                Some(TokenSide::Yes)
            } else if market.no_token_id.as_deref() == Some(asset_id) {
                Some(TokenSide::No)
            } else {
                None
            }
        })
}

fn map_market(value: Value) -> Result<Option<Market>> {
    let market_id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let slug = value
        .get("slug")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let condition_id = value
        .get("conditionId")
        .or_else(|| value.get("condition_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let question = value
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if market_id.is_empty() || question.is_empty() {
        return Ok(None);
    }

    let spec = match parse_market_spec(&question) {
        Ok(spec) => spec,
        Err(_) => return Ok(None),
    };

    let best_bid = first_f64(&value, &["bestBid", "best_bid"]);
    let best_ask = first_f64(&value, &["bestAsk", "best_ask"]);
    let active = value.get("active").and_then(Value::as_bool).unwrap_or(true);
    let expires_at = value
        .get("endDateIso")
        .and_then(Value::as_str)
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let tokens = value
        .get("tokens")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let yes_token_id = tokens
        .iter()
        .find(|token| token.get("outcome").and_then(Value::as_str) == Some("Yes"))
        .and_then(|token| token.get("token_id").or_else(|| token.get("tokenId")))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let no_token_id = tokens
        .iter()
        .find(|token| token.get("outcome").and_then(Value::as_str) == Some("No"))
        .and_then(|token| token.get("token_id").or_else(|| token.get("tokenId")))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Ok(Some(Market {
        market_id,
        condition_id,
        slug,
        question,
        yes_token_id,
        no_token_id,
        spec,
        best_bid,
        best_ask,
        active,
        expires_at,
    }))
}

fn first_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| match value.get(*key) {
        Some(Value::String(raw)) => raw.parse().ok(),
        Some(Value::Number(number)) => number.as_f64(),
        _ => None,
    })
}

pub fn parse_market_spec(question: &str) -> Result<MarketSpec> {
    let daily_re = Regex::new(
        r"(?i)will the (?P<kind>high|low) temperature in (?P<city>.+?) on (?P<date>[A-Za-z]+\s+\d{1,2}(?:,\s*\d{4})?) be above (?P<threshold>-?\d+(?:\.\d+)?)\s*°?\s*C",
    )?;
    if let Some(captures) = daily_re.captures(question) {
        let city = captures["city"].trim().to_string();
        let target_date = parse_human_date(&captures["date"])?;
        let threshold_c = captures["threshold"].parse::<f64>()?;
        let kind = match captures["kind"].to_ascii_lowercase().as_str() {
            "high" => WeatherMarketKind::DailyHigh { threshold_c },
            "low" => WeatherMarketKind::DailyLow { threshold_c },
            _ => bail!("unsupported daily temperature kind"),
        };
        return Ok(MarketSpec {
            city,
            station_id: None,
            target_date,
            kind,
        });
    }

    let threshold_re = Regex::new(
        r"(?i)will (?P<city>.+?) have (?P<metric>precipitation|rainfall) (?P<op>above|below) (?P<threshold>-?\d+(?:\.\d+)?)\s*mm on (?P<date>[A-Za-z]+\s+\d{1,2}(?:,\s*\d{4})?)",
    )?;
    if let Some(captures) = threshold_re.captures(question) {
        let city = captures["city"].trim().to_string();
        let target_date = parse_human_date(&captures["date"])?;
        let threshold = captures["threshold"].parse::<f64>()?;
        let op = match captures["op"].to_ascii_lowercase().as_str() {
            "above" => ComparisonOp::Above,
            "below" => ComparisonOp::Below,
            _ => bail!("unsupported comparison operator"),
        };
        return Ok(MarketSpec {
            city,
            station_id: None,
            target_date,
            kind: WeatherMarketKind::Threshold {
                metric: WeatherMetric::PrecipitationMm,
                op,
                threshold,
            },
        });
    }

    bail!("unsupported weather market question: {question}")
}

fn parse_human_date(raw: &str) -> Result<NaiveDate> {
    let normalized = raw.trim();
    for format in ["%B %d, %Y", "%B %d %Y", "%B %d"] {
        if let Ok(date) = NaiveDate::parse_from_str(normalized, format) {
            return Ok(if format == "%B %d" {
                let current_year = Utc::now().year();
                NaiveDate::from_ymd_opt(current_year, date.month(), date.day())
                    .context("invalid month/day combination")?
            } else {
                date
            });
        }
    }
    bail!("unable to parse date {normalized}")
}

#[derive(Debug, Deserialize)]
struct _Unused;

#[cfg(test)]
mod tests {
    use super::parse_market_spec;
    use chrono::NaiveDate;
    use domain_core::WeatherMarketKind;

    #[test]
    fn parse_high_temperature_question() {
        let spec = parse_market_spec(
            "Will the high temperature in New York on April 2, 2026 be above 20 C?",
        )
        .expect("parse");
        assert_eq!(spec.city, "New York");
        assert_eq!(
            spec.target_date,
            NaiveDate::from_ymd_opt(2026, 4, 2).expect("date")
        );
        assert_eq!(
            spec.kind,
            WeatherMarketKind::DailyHigh { threshold_c: 20.0 }
        );
    }
}
