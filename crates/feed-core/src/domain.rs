use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Rss,
    Atom,
    Html,
    Browser,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    Approved,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    New,
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateValidationStatus {
    Running,
    Valid,
    NeedsReview,
    Invalid,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDecision {
    Activated,
    Rejected,
    KeptForReview,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDecisionMode {
    Automatic,
    Operator,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawItemStatus {
    Pending,
    Normalized,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    MarkdownJson,
    Jsonl,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportLayout {
    ByCompanyDate,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStatus {
    Public,
    Private,
    StateOwned,
    Cooperative,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    Active,
    Acquired,
    Inactive,
    #[default]
    Unknown,
}

macro_rules! string_enum {
    ($type:ty, $name:literal, { $($variant:path => $value:literal),+ $(,)? }) => {
        impl $type {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($variant => $value),+
                }
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $type {
            type Err = EnumParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok($variant)),+,
                    _ => Err(EnumParseError {
                        enum_name: $name,
                        value: value.to_owned(),
                    }),
                }
            }
        }
    };
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid {enum_name} value: {value}")]
pub struct EnumParseError {
    enum_name: &'static str,
    value: String,
}

impl EnumParseError {
    pub(crate) fn new(enum_name: &'static str, value: impl Into<String>) -> Self {
        Self {
            enum_name,
            value: value.into(),
        }
    }
}

string_enum!(SourceKind, "source kind", {
    SourceKind::Rss => "rss",
    SourceKind::Atom => "atom",
    SourceKind::Html => "html",
    SourceKind::Browser => "browser",
});
string_enum!(SourceStatus, "source status", {
    SourceStatus::Approved => "approved",
    SourceStatus::Disabled => "disabled",
});
string_enum!(CandidateStatus, "candidate status", {
    CandidateStatus::New => "new",
    CandidateStatus::Accepted => "accepted",
    CandidateStatus::Rejected => "rejected",
});
string_enum!(CandidateValidationStatus, "candidate validation status", {
    CandidateValidationStatus::Running => "running",
    CandidateValidationStatus::Valid => "valid",
    CandidateValidationStatus::NeedsReview => "needs_review",
    CandidateValidationStatus::Invalid => "invalid",
    CandidateValidationStatus::Failed => "failed",
    CandidateValidationStatus::Cancelled => "cancelled",
});
string_enum!(CandidateDecision, "candidate decision", {
    CandidateDecision::Activated => "activated",
    CandidateDecision::Rejected => "rejected",
    CandidateDecision::KeptForReview => "kept_for_review",
});
string_enum!(CandidateDecisionMode, "candidate decision mode", {
    CandidateDecisionMode::Automatic => "automatic",
    CandidateDecisionMode::Operator => "operator",
});
string_enum!(RawItemStatus, "raw item status", {
    RawItemStatus::Pending => "pending",
    RawItemStatus::Normalized => "normalized",
    RawItemStatus::Failed => "failed",
    RawItemStatus::Skipped => "skipped",
});
string_enum!(RunStatus, "run status", {
    RunStatus::Running => "running",
    RunStatus::Completed => "completed",
    RunStatus::Failed => "failed",
    RunStatus::Cancelled => "cancelled",
});
string_enum!(ExportFormat, "export format", {
    ExportFormat::MarkdownJson => "markdown_json",
    ExportFormat::Jsonl => "jsonl",
});
string_enum!(ExportLayout, "export layout", {
    ExportLayout::ByCompanyDate => "by_company_date",
});
string_enum!(OwnershipStatus, "ownership status", {
    OwnershipStatus::Public => "public",
    OwnershipStatus::Private => "private",
    OwnershipStatus::StateOwned => "state_owned",
    OwnershipStatus::Cooperative => "cooperative",
    OwnershipStatus::Unknown => "unknown",
});
string_enum!(LifecycleStatus, "lifecycle status", {
    LifecycleStatus::Active => "active",
    LifecycleStatus::Acquired => "acquired",
    LifecycleStatus::Inactive => "inactive",
    LifecycleStatus::Unknown => "unknown",
});

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn database_strings_round_trip() {
        let values = [
            (SourceKind::Rss, "rss"),
            (SourceKind::Atom, "atom"),
            (SourceKind::Html, "html"),
            (SourceKind::Browser, "browser"),
        ];

        for (value, expected) in values {
            assert_eq!(value.as_str(), expected);
            assert_eq!(SourceKind::from_str(expected), Ok(value));
        }
    }
}
