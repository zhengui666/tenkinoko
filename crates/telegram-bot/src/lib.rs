use anyhow::{Context, Result};
use chrono::Utc;
use domain_core::{
    CommandInboxMessage, Event, JobStatus, RiskState, TelegramDeliveryStatus, TelegramOutboxMessage,
};
use scheduler_core::enqueue_job;
use serde::{Deserialize, Serialize};
use storage_rocksdb::Storage;
use uuid::Uuid;

#[derive(Clone)]
pub struct TelegramNotifier {
    client: reqwest::Client,
    bot_token: Option<String>,
    chat_id: Option<String>,
    readonly_chat_ids: Vec<String>,
    admin_chat_ids: Vec<String>,
    storage: Storage,
}

impl TelegramNotifier {
    pub fn new(
        bot_token: Option<String>,
        chat_id: Option<String>,
        readonly_chat_ids: Vec<String>,
        admin_chat_ids: Vec<String>,
        storage: Storage,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            bot_token,
            chat_id,
            readonly_chat_ids,
            admin_chat_ids,
            storage,
        }
    }

    pub async fn notify(&self, body: &str) -> Result<()> {
        let message = self.storage.enqueue_telegram_message(body)?;
        let _ = self.try_deliver(message).await?;
        Ok(())
    }

    pub async fn flush_outbox(&self, max_attempts: u32) -> Result<usize> {
        let mut delivered = 0usize;
        for message in self.storage.list_telegram_messages()? {
            if !matches!(
                message.status,
                TelegramDeliveryStatus::Pending | TelegramDeliveryStatus::Failed
            ) {
                continue;
            }
            if message.attempts >= max_attempts {
                let mut dead = message.clone();
                dead.status = TelegramDeliveryStatus::DeadLetter;
                dead.updated_at = Utc::now();
                self.storage.save_telegram_message(&dead)?;
                continue;
            }
            if self.try_deliver(message).await? {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    pub async fn poll_commands_once(&self) -> Result<usize> {
        let Some(bot_token) = self.bot_token.as_ref() else {
            return Ok(0);
        };

        let updates = self
            .client
            .get(format!(
                "https://api.telegram.org/bot{bot_token}/getUpdates?timeout=1"
            ))
            .send()
            .await
            .context("failed to poll Telegram getUpdates")?
            .error_for_status()
            .context("Telegram getUpdates returned non-success status")?
            .json::<GetUpdatesResponse>()
            .await
            .context("failed to decode Telegram updates response")?;

        let existing_commands = self.storage.list_commands()?;
        let mut processed = 0usize;

        for update in updates.result {
            let Some(message) = update.message else {
                continue;
            };
            let Some(chat_id) = message.chat.as_ref().map(|chat| chat.id.clone()) else {
                continue;
            };
            if !self.is_read_authorized(&chat_id) {
                continue;
            }
            let Some(text) = message.text else {
                continue;
            };
            if !text.starts_with('/') {
                continue;
            }
            let source = format!("telegram:{}", update.update_id);
            if existing_commands.iter().any(|entry| entry.source == source) {
                continue;
            }

            let mut command = CommandInboxMessage {
                id: Uuid::new_v4(),
                source,
                command: text.clone(),
                received_at: Utc::now(),
                processed_at: None,
            };
            self.storage.save_command(&command)?;
            self.storage
                .append_event(&Event::CommandReceived(command.clone()))?;

            let reply = self.handle_command(&chat_id, &command.command).await?;
            self.send_direct_reply(&reply).await?;

            command.processed_at = Some(Utc::now());
            self.storage.save_command(&command)?;
            processed += 1;
        }

        Ok(processed)
    }

    async fn handle_command(&self, chat_id: &str, raw: &str) -> Result<String> {
        let command = raw.trim();
        if command.eq_ignore_ascii_case("/status") {
            return self.render_status();
        }
        if command.eq_ignore_ascii_case("/positions") {
            return self.render_positions();
        }
        if command.eq_ignore_ascii_case("/risk") {
            return self.render_risk();
        }
        if command.eq_ignore_ascii_case("/pnl") {
            return self.render_pnl();
        }
        if command.eq_ignore_ascii_case("/daily") {
            return self.render_daily_report();
        }
        if command.eq_ignore_ascii_case("/jobs") {
            return self.render_jobs();
        }
        if command.eq_ignore_ascii_case("/commands") {
            return self.render_commands();
        }
        if command.eq_ignore_ascii_case("/pause open") {
            if !self.is_admin_authorized(chat_id) {
                return Ok("admin authorization required".to_string());
            }
            self.storage.set_risk_state(&RiskState::HaltOpen)?;
            return Ok("risk state set to HaltOpen".to_string());
        }
        if command.eq_ignore_ascii_case("/resume open") {
            if !self.is_admin_authorized(chat_id) {
                return Ok("admin authorization required".to_string());
            }
            self.storage.set_risk_state(&RiskState::Normal)?;
            return Ok("risk state set to Normal".to_string());
        }
        if let Some(slug) = command.strip_prefix("/market ").map(str::trim) {
            return self.render_market(slug);
        }
        if let Some(slug) = command.strip_prefix("/runtime ").map(str::trim) {
            return self.render_runtime(slug);
        }
        if let Some(identifier) = command.strip_prefix("/why ").map(str::trim) {
            return self.render_why(identifier);
        }
        if let Some(position_id) = command.strip_prefix("/close ").map(str::trim) {
            if !self.is_admin_authorized(chat_id) {
                return Ok("admin authorization required".to_string());
            }
            let job = enqueue_job(&self.storage, "manual_close", position_id, 0)?;
            return Ok(format!(
                "manual close job queued | job_id={} | position_id={}",
                job.id, position_id
            ));
        }

        Ok("unsupported command".to_string())
    }

    fn render_status(&self) -> Result<String> {
        let open_positions = self
            .storage
            .list_positions()?
            .into_iter()
            .filter(|position| position.exit_reason.is_none())
            .count();
        let live_orders = self
            .storage
            .list_orders()?
            .into_iter()
            .filter(|order| matches!(order.status, domain_core::OrderStatus::Sent))
            .count();
        let pending_jobs = self
            .storage
            .list_jobs()?
            .into_iter()
            .filter(|job| matches!(job.status, JobStatus::Pending | JobStatus::Failed))
            .count();
        let risk_state = self.storage.get_risk_state()?.unwrap_or(RiskState::Normal);

        Ok(format!(
            "status | risk={risk_state:?} | open_positions={open_positions} | live_orders={live_orders} | pending_jobs={pending_jobs}"
        ))
    }

    fn render_positions(&self) -> Result<String> {
        let positions = self.storage.list_positions()?;
        let open = positions
            .into_iter()
            .filter(|position| position.exit_reason.is_none())
            .map(|position| {
                format!(
                    "{} | market={} | side={:?} | qty={:.4} | avg={:.4} | pending_close={:.4}",
                    position.position_id,
                    position.market_id,
                    position.side,
                    position.quantity_shares,
                    position.average_price,
                    position.pending_close_shares
                )
            })
            .collect::<Vec<_>>();
        if open.is_empty() {
            Ok("positions | none".to_string())
        } else {
            Ok(format!("positions\n{}", open.join("\n")))
        }
    }

    fn render_risk(&self) -> Result<String> {
        let risk_state = self.storage.get_risk_state()?.unwrap_or(RiskState::Normal);
        let decisions = self.storage.list_jobs()?.len();
        Ok(format!(
            "risk | state={risk_state:?} | queued_jobs={decisions}"
        ))
    }

    fn render_pnl(&self) -> Result<String> {
        let positions = self.storage.list_positions()?;
        let open_positions = positions
            .iter()
            .filter(|position| position.exit_reason.is_none())
            .count();
        let gross_cost = positions
            .iter()
            .filter(|position| position.exit_reason.is_none())
            .map(|position| position.size_usd)
            .sum::<f64>();
        Ok(format!(
            "pnl | open_positions={} | tracked_cost_basis_usd={:.2}",
            open_positions, gross_cost
        ))
    }

    fn render_market(&self, slug_or_city: &str) -> Result<String> {
        let needle = slug_or_city.to_ascii_lowercase();
        let markets = self.storage.list_markets()?;
        let Some(market) = markets.iter().find(|market| {
            market.slug.to_ascii_lowercase() == needle
                || market.spec.city.to_ascii_lowercase() == needle
        }) else {
            return Ok("market | not found".to_string());
        };
        Ok(format!(
            "market | id={} | slug={} | city={} | best_bid={:?} | best_ask={:?} | question={}",
            market.market_id,
            market.slug,
            market.spec.city,
            market.best_bid,
            market.best_ask,
            market.question
        ))
    }

    fn render_why(&self, identifier: &str) -> Result<String> {
        let signals = self.storage.list_signals()?;
        let llm = self.storage.list_llm_insights()?;
        let orders = self.storage.list_orders()?;

        let signal_match = signals
            .iter()
            .rev()
            .find(|signal| signal.market_id == identifier);
        if let Some(signal) = signal_match {
            let llm_text = llm
                .iter()
                .rev()
                .find(|insight| insight.market_id == signal.market_id)
                .map(|insight| format!(" | analyst={}", insight.summary))
                .unwrap_or_default();
            return Ok(format!(
                "why | market={} | side={:?} | edge_bps={} | reason={}{}",
                signal.market_id, signal.side, signal.edge_bps, signal.reason, llm_text
            ));
        }

        if let Some(order) = orders.iter().find(|order| {
            order.intent.id.to_string() == identifier
                || order.venue_order_id.as_deref() == Some(identifier)
        }) {
            return Ok(format!(
                "why | order={} | market={} | action={:?} | side={:?} | status={:?} | qty={:.4}",
                order.intent.id,
                order.intent.market_id,
                order.intent.action,
                order.intent.side,
                order.status,
                order.intent.quantity_shares
            ));
        }

        Ok("why | no matching market/order found".to_string())
    }

    fn render_daily_report(&self) -> Result<String> {
        let positions = self.storage.list_positions()?;
        let orders = self.storage.list_orders()?;
        let insights = self.storage.list_llm_insights()?;
        Ok(format!(
            "daily | open_positions={} | live_orders={} | llm_notes={} | risk={:?}",
            positions
                .iter()
                .filter(|position| position.exit_reason.is_none())
                .count(),
            orders
                .iter()
                .filter(|order| matches!(order.status, domain_core::OrderStatus::Sent))
                .count(),
            insights.len(),
            self.storage.get_risk_state()?.unwrap_or(RiskState::Normal)
        ))
    }

    fn render_jobs(&self) -> Result<String> {
        let jobs = self.storage.list_jobs()?;
        let lines = jobs
            .iter()
            .rev()
            .take(10)
            .map(|job| {
                format!(
                    "{} | kind={} | status={:?} | attempts={}",
                    job.id, job.kind, job.status, job.attempts
                )
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            Ok("jobs | none".to_string())
        } else {
            Ok(format!("jobs\n{}", lines.join("\n")))
        }
    }

    fn render_commands(&self) -> Result<String> {
        let commands = self.storage.list_commands()?;
        let lines = commands
            .iter()
            .rev()
            .take(10)
            .map(|command| {
                format!(
                    "{} | source={} | command={} | processed={}",
                    command.id,
                    command.source,
                    command.command,
                    command.processed_at.is_some()
                )
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            Ok("commands | none".to_string())
        } else {
            Ok(format!("commands\n{}", lines.join("\n")))
        }
    }

    fn render_runtime(&self, identifier: &str) -> Result<String> {
        let needle = identifier.to_ascii_lowercase();
        let markets = self.storage.list_markets()?;
        let market_ids = markets
            .iter()
            .filter(|market| {
                market.market_id.to_ascii_lowercase() == needle
                    || market.slug.to_ascii_lowercase() == needle
                    || market.spec.city.to_ascii_lowercase() == needle
            })
            .map(|market| market.market_id.clone())
            .collect::<Vec<_>>();
        let runtimes = self.storage.list_market_runtime()?;
        let Some(runtime) = runtimes
            .iter()
            .filter(|runtime| {
                market_ids
                    .iter()
                    .any(|market_id| market_id == &runtime.market_id)
            })
            .max_by_key(|runtime| runtime.captured_at)
        else {
            return Ok("runtime | not found".to_string());
        };
        Ok(format!(
            "runtime | market={} | yes_mid={:?} | no_mid={:?} | yes_last={:?} | no_last={:?}",
            runtime.market_id,
            runtime.yes_midpoint,
            runtime.no_midpoint,
            runtime.yes_last_trade,
            runtime.no_last_trade
        ))
    }

    async fn try_deliver(&self, message: TelegramOutboxMessage) -> Result<bool> {
        let (Some(bot_token), Some(chat_id)) = (&self.bot_token, &self.chat_id) else {
            return Ok(false);
        };
        let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
        let result = self
            .client
            .post(url)
            .json(&SendMessageRequest {
                chat_id: chat_id.clone(),
                text: message.body.clone(),
            })
            .send()
            .await;

        match result {
            Ok(response) => {
                response
                    .error_for_status()
                    .context("Telegram API returned non-success status")?;
                let mut delivered = message.clone();
                delivered.status = TelegramDeliveryStatus::Sent;
                delivered.attempts += 1;
                delivered.last_error = None;
                delivered.updated_at = Utc::now();
                self.storage.save_telegram_message(&delivered)?;
                self.storage
                    .append_event(&Event::TelegramDelivered(delivered.id))?;
                Ok(true)
            }
            Err(error) => {
                let mut failed = message.clone();
                failed.status = TelegramDeliveryStatus::Failed;
                failed.attempts += 1;
                failed.last_error = Some(error.to_string());
                failed.updated_at = Utc::now();
                self.storage.save_telegram_message(&failed)?;
                Ok(false)
            }
        }
    }

    async fn send_direct_reply(&self, body: &str) -> Result<()> {
        let (Some(bot_token), Some(chat_id)) = (&self.bot_token, &self.chat_id) else {
            return Ok(());
        };
        self.client
            .post(format!(
                "https://api.telegram.org/bot{bot_token}/sendMessage"
            ))
            .json(&SendMessageRequest {
                chat_id: chat_id.clone(),
                text: body.to_string(),
            })
            .send()
            .await
            .context("failed to send Telegram command reply")?
            .error_for_status()
            .context("Telegram command reply returned non-success status")?;
        Ok(())
    }

    fn is_read_authorized(&self, chat_id: &str) -> bool {
        if !self.readonly_chat_ids.is_empty() {
            return self
                .readonly_chat_ids
                .iter()
                .any(|allowed| allowed == chat_id);
        }
        self.chat_id.as_deref() == Some(chat_id)
    }

    fn is_admin_authorized(&self, chat_id: &str) -> bool {
        if !self.admin_chat_ids.is_empty() {
            return self.admin_chat_ids.iter().any(|allowed| allowed == chat_id);
        }
        self.is_read_authorized(chat_id)
    }
}

#[derive(Serialize)]
struct SendMessageRequest {
    chat_id: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct GetUpdatesResponse {
    result: Vec<Update>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<IncomingMessage>,
}

#[derive(Debug, Deserialize)]
struct IncomingMessage {
    text: Option<String>,
    chat: Option<IncomingChat>,
}

#[derive(Debug, Deserialize)]
struct IncomingChat {
    id: String,
}
