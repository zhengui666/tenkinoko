use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use domain_core::{JobRecord, JobStatus, SchedulerCheckpoint};
use storage_rocksdb::Storage;
use tokio::time::{Duration, sleep};
use uuid::Uuid;

pub async fn sleep_until_next_cycle(interval_secs: u64) {
    sleep(Duration::from_secs(interval_secs)).await;
}

pub fn enqueue_job(
    storage: &Storage,
    kind: impl Into<String>,
    payload: impl Into<String>,
    delay_secs: u64,
) -> Result<JobRecord> {
    let now = Utc::now();
    let job = JobRecord {
        id: Uuid::new_v4(),
        kind: kind.into(),
        payload: payload.into(),
        status: JobStatus::Pending,
        attempts: 0,
        not_before: now + ChronoDuration::seconds(delay_secs as i64),
        updated_at: now,
    };
    storage.save_job(&job)?;
    storage.append_event(&domain_core::Event::JobQueued(job.clone()))?;
    Ok(job)
}

pub fn claim_due_jobs(storage: &Storage, max_jobs: usize) -> Result<Vec<JobRecord>> {
    let now = Utc::now();
    let mut claimed = Vec::new();
    for mut job in storage.list_jobs()? {
        if claimed.len() >= max_jobs {
            break;
        }
        if !matches!(job.status, JobStatus::Pending | JobStatus::Failed) || job.not_before > now {
            continue;
        }
        job.status = JobStatus::Running;
        job.attempts += 1;
        job.updated_at = now;
        storage.save_job(&job)?;
        claimed.push(job);
    }
    Ok(claimed)
}

pub fn complete_job(storage: &Storage, job: &JobRecord) -> Result<()> {
    let mut updated = job.clone();
    updated.status = JobStatus::Completed;
    updated.updated_at = Utc::now();
    storage.save_job(&updated)
}

pub fn fail_job(storage: &Storage, job: &JobRecord, retry_delay_secs: u64) -> Result<()> {
    let mut updated = job.clone();
    updated.status = JobStatus::Failed;
    updated.not_before = Utc::now() + ChronoDuration::seconds(retry_delay_secs as i64);
    updated.updated_at = Utc::now();
    storage.save_job(&updated)
}

pub fn save_cycle_checkpoint(
    storage: &Storage,
    name: &str,
    interval_secs: u64,
) -> Result<SchedulerCheckpoint> {
    let now = Utc::now();
    let checkpoint = SchedulerCheckpoint {
        name: name.to_string(),
        last_run_at: now,
        next_run_at: now + ChronoDuration::seconds(interval_secs as i64),
    };
    storage.save_scheduler_checkpoint(&checkpoint)?;
    Ok(checkpoint)
}
