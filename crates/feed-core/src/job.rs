use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::domain::EnumParseError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    DiscoverCompany,
    ValidateCandidate,
    CrawlSource,
    CrawlContent,
    ExtractCompanyNews,
    ExportTarget,
    NormalizeBackfill,
}

impl JobType {
    pub const ALL: [Self; 7] = [
        Self::DiscoverCompany,
        Self::ValidateCandidate,
        Self::CrawlSource,
        Self::CrawlContent,
        Self::ExtractCompanyNews,
        Self::ExportTarget,
        Self::NormalizeBackfill,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiscoverCompany => "discover_company",
            Self::ValidateCandidate => "validate_candidate",
            Self::CrawlSource => "crawl_source",
            Self::CrawlContent => "crawl_content",
            Self::ExtractCompanyNews => "extract_company_news",
            Self::ExportTarget => "export_target",
            Self::NormalizeBackfill => "normalize_backfill",
        }
    }
}

impl fmt::Display for JobType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for JobType {
    type Err = EnumParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "discover_company" => Ok(Self::DiscoverCompany),
            "validate_candidate" => Ok(Self::ValidateCandidate),
            "crawl_source" => Ok(Self::CrawlSource),
            "crawl_content" => Ok(Self::CrawlContent),
            "extract_company_news" => Ok(Self::ExtractCompanyNews),
            "export_target" => Ok(Self::ExportTarget),
            "normalize_backfill" => Ok(Self::NormalizeBackfill),
            _ => Err(EnumParseError::new("job type", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for JobStatus {
    type Err = EnumParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(EnumParseError::new("job status", value)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub job_type: JobType,
    pub job_key: String,
    pub status: JobStatus,
    pub priority: i16,
    pub run_after: DateTime<Utc>,
    pub attempt_count: i32,
    pub max_attempts: i32,
    pub company_id: Option<Uuid>,
    pub candidate_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub export_target_id: Option<Uuid>,
    pub payload: Value,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimedJob {
    pub job: Job,
    pub worker_id: String,
    pub lease_token: Uuid,
    pub lease_until: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobSpec {
    pub job_type: JobType,
    pub job_key: String,
    pub run_after: DateTime<Utc>,
    pub priority: i16,
    pub max_attempts: i32,
    pub company_id: Option<Uuid>,
    pub candidate_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub export_target_id: Option<Uuid>,
    pub payload: Value,
}

impl JobSpec {
    pub fn new(job_type: JobType, job_key: impl Into<String>, run_after: DateTime<Utc>) -> Self {
        Self {
            job_type,
            job_key: job_key.into(),
            run_after,
            priority: 0,
            max_attempts: 5,
            company_id: None,
            candidate_id: None,
            source_id: None,
            export_target_id: None,
            payload: Value::Object(Default::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn job_types_round_trip_database_values() {
        for job_type in JobType::ALL {
            assert_eq!(JobType::from_str(job_type.as_str()), Ok(job_type));
        }
    }
}
