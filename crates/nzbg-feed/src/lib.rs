pub mod coordinator;
pub mod fetch;
pub mod rss;

pub use coordinator::{FeedCommand, FeedCoordinator, FeedHandle};

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use nzbg_core::models::DupMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedConfig {
    pub id: u32,
    pub name: String,
    pub url: String,
    pub filter: String,
    pub interval_min: u32,
    pub backlog: bool,
    pub pause_nzb: bool,
    pub category: String,
    pub priority: i32,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItemInfo {
    pub title: String,
    pub url: String,
    pub filename: String,
    pub category: String,
    pub size: u64,
    pub time: DateTime<Utc>,
    pub description: String,
    pub season: Option<String>,
    pub episode: Option<String>,
    pub imdb_id: Option<String>,
    pub rageid: Option<u32>,
    pub tvdbid: Option<u32>,
    pub tvmazeid: Option<u32>,
    pub rating: Option<i64>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: DupMode,
    pub status: FeedItemStatus,
    pub added_nzb_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedItemStatus {
    Unknown,
    Backlog,
    Fetched,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedInfo {
    pub id: u32,
    pub name: String,
    pub url: String,
    pub last_update: Option<DateTime<Utc>>,
    pub next_update: Option<DateTime<Utc>>,
    pub status: FeedStatus,
    pub item_count: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedStatus {
    Idle,
    Fetching,
    Error,
}

#[derive(Debug, Clone)]
pub struct FeedFilter {
    pub rules: Vec<FilterRule>,
}

#[derive(Debug, Clone)]
pub struct FilterRule {
    pub action: FilterAction,
    pub conditions: Vec<FilterCondition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterAction {
    Accept,
    Reject,
    Require,
    Options,
    Comment,
}

#[derive(Debug, Clone)]
pub struct FilterCondition {
    pub field: FilterField,
    pub pattern: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterField {
    Title,
    Category,
    Size,
    Age,
    Imdb,
    Season,
    Episode,
    Rating,
    Genre,
    Tag,
    Priority,
    Pause,
    CategoryOverride,
    DupeKey,
    DupeScore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterResult {
    pub action: FilterAction,
    pub category: Option<String>,
    pub priority: Option<i32>,
    pub pause: Option<bool>,
    pub dupe_key: Option<String>,
    pub dupe_score: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "filter parse error on line {}: {}",
            self.line, self.message
        )
    }
}

impl std::error::Error for FilterParseError {}

impl FeedFilter {
    pub fn parse(filter_text: &str) -> Result<Self, FilterParseError> {
        let mut rules = Vec::new();
        for (index, line) in filter_text.lines().enumerate() {
            let line_no = index + 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let (action, rest) = parse_action(trimmed, line_no)?;
            if action == FilterAction::Comment {
                continue;
            }

            let conditions = parse_conditions(rest, line_no)?;
            rules.push(FilterRule { action, conditions });
        }
        Ok(Self { rules })
    }

    pub fn evaluate(&self, item: &FeedItemInfo) -> FilterResult {
        for rule in &self.rules {
            if rule.matches(item) {
                let mut result = rule.option_overrides(item);
                result.action = rule.action;
                return result;
            }
        }
        FilterResult::reject()
    }
}

impl FilterRule {
    pub fn matches(&self, item: &FeedItemInfo) -> bool {
        if self.conditions.is_empty() {
            return true;
        }
        self.conditions
            .iter()
            .all(|condition| condition.matches(item))
    }

    pub fn option_overrides(&self, item: &FeedItemInfo) -> FilterResult {
        let mut result = FilterResult::reject();
        for condition in &self.conditions {
            match condition.field {
                FilterField::CategoryOverride => {
                    result.category = Some(condition.pattern.clone());
                }
                FilterField::Priority => {
                    if let Ok(priority) = condition.pattern.parse() {
                        result.priority = Some(priority);
                    }
                }
                FilterField::Pause => {
                    result.pause = parse_bool(&condition.pattern).ok();
                }
                FilterField::DupeKey => {
                    result.dupe_key = Some(condition.pattern.clone());
                }
                FilterField::DupeScore => {
                    if let Ok(score) = condition.pattern.parse() {
                        result.dupe_score = Some(score);
                    }
                }
                _ => {
                    let _ = item;
                }
            }
        }
        result
    }
}

impl FilterCondition {
    pub fn matches(&self, item: &FeedItemInfo) -> bool {
        match self.field {
            FilterField::Title => match_regex(&self.pattern, &item.title),
            FilterField::Category => match_wildcard(&self.pattern, &item.category),
            FilterField::Size => match_number_comparison(&self.pattern, item.size as i64),
            FilterField::Age => {
                let age_days = (Utc::now() - item.time).num_days();
                match_number_comparison(&self.pattern, age_days)
            }
            FilterField::Imdb => item
                .imdb_id
                .as_deref()
                .is_some_and(|id| match_regex(&self.pattern, id)),
            FilterField::Season => item
                .season
                .as_deref()
                .is_some_and(|season| match_regex(&self.pattern, season)),
            FilterField::Episode => item
                .episode
                .as_deref()
                .is_some_and(|episode| match_regex(&self.pattern, episode)),
            FilterField::Rating => item
                .rating
                .is_some_and(|r| match_number_comparison(&self.pattern, r)),
            FilterField::Genre => item.genres.iter().any(|g| match_wildcard(&self.pattern, g)),
            FilterField::Tag => item.tags.iter().any(|t| match_wildcard(&self.pattern, t)),
            FilterField::Priority => true,
            FilterField::Pause => true,
            FilterField::CategoryOverride => true,
            FilterField::DupeKey => true,
            FilterField::DupeScore => true,
        }
    }
}

fn parse_action(line: &str, line_no: usize) -> Result<(FilterAction, &str), FilterParseError> {
    let (raw_action, rest) = line.split_once(':').ok_or_else(|| FilterParseError {
        line: line_no,
        message: "missing action separator ':'".to_string(),
    })?;
    let action = match raw_action.trim() {
        "A" | "Accept" => FilterAction::Accept,
        "R" | "Reject" => FilterAction::Reject,
        "Q" | "Require" => FilterAction::Require,
        "O" | "Options" => FilterAction::Options,
        "C" | "Comment" => FilterAction::Comment,
        other => {
            return Err(FilterParseError {
                line: line_no,
                message: format!("unknown action '{other}'"),
            });
        }
    };
    Ok((action, rest.trim()))
}

fn parse_conditions(text: &str, line_no: usize) -> Result<Vec<FilterCondition>, FilterParseError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }

    let mut conditions = Vec::new();
    for token in text.split_whitespace() {
        let (raw_field, pattern) = token.split_once('(').ok_or_else(|| FilterParseError {
            line: line_no,
            message: format!("missing '(' in condition '{token}'"),
        })?;
        if !pattern.ends_with(')') {
            return Err(FilterParseError {
                line: line_no,
                message: format!("missing ')' in condition '{token}'"),
            });
        }
        let pattern = &pattern[..pattern.len() - 1];
        let field = parse_field(raw_field, line_no)?;
        conditions.push(FilterCondition {
            field,
            pattern: pattern.to_string(),
        });
    }
    Ok(conditions)
}

fn parse_field(value: &str, line_no: usize) -> Result<FilterField, FilterParseError> {
    match value.trim() {
        "title" => Ok(FilterField::Title),
        "category" => Ok(FilterField::Category),
        "size" => Ok(FilterField::Size),
        "age" => Ok(FilterField::Age),
        "imdb" => Ok(FilterField::Imdb),
        "season" => Ok(FilterField::Season),
        "episode" => Ok(FilterField::Episode),
        "rating" => Ok(FilterField::Rating),
        "genre" => Ok(FilterField::Genre),
        "tag" => Ok(FilterField::Tag),
        "priority" => Ok(FilterField::Priority),
        "pause" => Ok(FilterField::Pause),
        "category:" => Ok(FilterField::CategoryOverride),
        "dupekey" => Ok(FilterField::DupeKey),
        "dupescore" => Ok(FilterField::DupeScore),
        other => Err(FilterParseError {
            line: line_no,
            message: format!("unknown field '{other}'"),
        }),
    }
}

fn parse_bool(value: &str) -> Result<bool, FilterParseError> {
    match value.trim().to_lowercase().as_str() {
        "yes" | "true" | "1" => Ok(true),
        "no" | "false" | "0" => Ok(false),
        other => Err(FilterParseError {
            line: 0,
            message: format!("invalid boolean '{other}'"),
        }),
    }
}

impl FilterResult {
    pub fn reject() -> Self {
        Self {
            action: FilterAction::Reject,
            category: None,
            priority: None,
            pause: None,
            dupe_key: None,
            dupe_score: None,
        }
    }
}

fn match_regex(pattern: &str, value: &str) -> bool {
    Regex::new(pattern).is_ok_and(|regex| regex.is_match(value))
}

fn match_wildcard(pattern: &str, value: &str) -> bool {
    let escaped = regex::escape(pattern).replace("\\*", ".*");
    let pattern = format!("^{escaped}$");
    match_regex(&pattern, value)
}

fn match_number_comparison(pattern: &str, value: i64) -> bool {
    let trimmed = pattern.trim();
    let (op, number) = if let Some(rest) = trimmed.strip_prefix(">=") {
        (">=", rest)
    } else if let Some(rest) = trimmed.strip_prefix("<=") {
        ("<=", rest)
    } else if let Some(rest) = trimmed.strip_prefix('>') {
        (">", rest)
    } else if let Some(rest) = trimmed.strip_prefix('<') {
        ("<", rest)
    } else if let Some(rest) = trimmed.strip_prefix('=') {
        ("=", rest)
    } else {
        ("=", trimmed)
    };

    let parsed = parse_size(number).unwrap_or(0);
    match op {
        ">=" => value >= parsed,
        "<=" => value <= parsed,
        ">" => value > parsed,
        "<" => value < parsed,
        "=" => value == parsed,
        _ => false,
    }
}

fn parse_size(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut number_part = String::new();
    let mut suffix_part = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number_part.push(ch);
        } else {
            suffix_part.push(ch);
        }
    }

    let number: f64 = number_part.parse().ok()?;
    let multiplier = match suffix_part.trim().to_lowercase().as_str() {
        "b" | "" => 1.0,
        "kb" | "k" => 1024.0,
        "mb" | "m" => 1024.0 * 1024.0,
        "gb" | "g" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * multiplier) as i64)
}

#[derive(Debug, Clone, Default)]
pub struct FeedHistoryDb {
    entries: HashMap<(u32, String), FeedHistoryEntry>,
    retention_days: u32,
}

#[derive(Debug, Clone)]
pub struct FeedHistoryEntry {
    pub status: FeedItemStatus,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl FeedHistoryDb {
    pub fn new(retention_days: u32) -> Self {
        Self {
            entries: HashMap::new(),
            retention_days,
        }
    }

    pub fn already_processed(&self, feed: &FeedConfig, item: &FeedItemInfo) -> bool {
        self.entries.contains_key(&(feed.id, item.url.clone()))
    }

    pub fn mark(&mut self, feed: &FeedConfig, item: &FeedItemInfo, status: FeedItemStatus) {
        let now = Utc::now();
        let entry = self
            .entries
            .entry((feed.id, item.url.clone()))
            .or_insert_with(|| FeedHistoryEntry {
                status,
                first_seen: now,
                last_seen: now,
            });
        entry.status = status;
        entry.last_seen = now;
    }

    pub fn purge_old(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::days(self.retention_days as i64);
        self.entries.retain(|_, entry| entry.last_seen > cutoff);
    }

    pub fn to_serializable(&self) -> Vec<(u32, String, FeedHistoryEntry)> {
        self.entries
            .iter()
            .map(|((feed_id, url), entry)| (*feed_id, url.clone(), entry.clone()))
            .collect()
    }

    pub fn load_entries(&mut self, entries: Vec<(u32, String, FeedHistoryEntry)>) {
        for (feed_id, url, entry) in entries {
            self.entries.insert((feed_id, url), entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item() -> FeedItemInfo {
        FeedItemInfo {
            title: "Show.S01E02.720p".to_string(),
            url: "https://example.com/file.nzb".to_string(),
            filename: "file.nzb".to_string(),
            category: "TV".to_string(),
            size: 700 * 1024 * 1024,
            time: Utc::now(),
            description: String::new(),
            season: Some("1".to_string()),
            episode: Some("2".to_string()),
            imdb_id: Some("tt1234567".to_string()),
            rageid: None,
            tvdbid: None,
            tvmazeid: None,
            rating: Some(75),
            genres: vec!["Drama".to_string(), "Sci-Fi".to_string()],
            tags: vec!["hd".to_string(), "x264".to_string()],
            dupe_key: String::new(),
            dupe_score: 0,
            dupe_mode: DupMode::Score,
            status: FeedItemStatus::Unknown,
            added_nzb_id: None,
        }
    }

    #[test]
    fn filter_age_matches_recent_item() {
        let filter = FeedFilter::parse("A: age(<30)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Accept);
    }

    #[test]
    fn filter_age_rejects_old_item() {
        let filter = FeedFilter::parse("A: age(<0)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn filter_rating_matches_above_threshold() {
        let filter = FeedFilter::parse("A: rating(>=50)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Accept);
    }

    #[test]
    fn filter_rating_rejects_below_threshold() {
        let filter = FeedFilter::parse("A: rating(>=90)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn filter_genre_matches_wildcard() {
        let filter = FeedFilter::parse("A: genre(Drama)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Accept);
    }

    #[test]
    fn filter_genre_rejects_no_match() {
        let filter = FeedFilter::parse("A: genre(Comedy)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn filter_tag_matches_wildcard() {
        let filter = FeedFilter::parse("A: tag(hd)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Accept);
    }

    #[test]
    fn filter_tag_rejects_no_match() {
        let filter = FeedFilter::parse("A: tag(hevc)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn parse_filter_accepts_conditions() {
        let filter = FeedFilter::parse("A: title(.*720p.*) category(TV)").expect("filter");
        assert_eq!(filter.rules.len(), 1);
        assert_eq!(filter.rules[0].conditions.len(), 2);
    }

    #[test]
    fn filter_evaluate_returns_reject_by_default() {
        let filter = FeedFilter::parse("").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn filter_accepts_matching_rule() {
        let filter = FeedFilter::parse("A: title(.*720p.*)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Accept);
    }

    #[test]
    fn filter_rejects_non_matching_rule() {
        let filter = FeedFilter::parse("A: title(.*1080p.*)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn filter_sets_overrides() {
        let filter =
            FeedFilter::parse("O: category:(Movies) priority(5) pause(yes) dupescore(100)")
                .expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Options);
        assert_eq!(result.category, Some("Movies".to_string()));
        assert_eq!(result.priority, Some(5));
        assert_eq!(result.pause, Some(true));
        assert_eq!(result.dupe_score, Some(100));
    }

    #[test]
    fn filter_requires_all_conditions() {
        let filter = FeedFilter::parse("A: title(.*720p.*) category(Movie)").expect("filter");
        let item = sample_item();
        let result = filter.evaluate(&item);
        assert_eq!(result.action, FilterAction::Reject);
    }

    #[test]
    fn parse_size_supports_units() {
        assert_eq!(parse_size("10MB"), Some(10 * 1024 * 1024));
        assert_eq!(
            parse_size("1.5GB"),
            Some((1.5 * 1024.0 * 1024.0 * 1024.0) as i64)
        );
    }

    #[test]
    fn history_serialization_roundtrip() {
        let mut history = FeedHistoryDb::new(30);
        let feed = FeedConfig {
            id: 1,
            name: "feed".to_string(),
            url: "http://example.com".to_string(),
            filter: String::new(),
            interval_min: 15,
            backlog: false,
            pause_nzb: false,
            category: "TV".to_string(),
            priority: 0,
            extensions: vec![],
        };
        let item = sample_item();
        history.mark(&feed, &item, FeedItemStatus::Fetched);

        let entries = history.to_serializable();
        assert_eq!(entries.len(), 1);

        let mut restored = FeedHistoryDb::new(30);
        restored.load_entries(entries);
        assert!(restored.already_processed(&feed, &item));
    }

    #[test]
    fn history_tracks_items_and_purges() {
        let mut history = FeedHistoryDb::new(1);
        let feed = FeedConfig {
            id: 1,
            name: "feed".to_string(),
            url: "http://example.com".to_string(),
            filter: String::new(),
            interval_min: 15,
            backlog: false,
            pause_nzb: false,
            category: "TV".to_string(),
            priority: 0,
            extensions: vec![],
        };
        let item = sample_item();
        history.mark(&feed, &item, FeedItemStatus::Fetched);
        assert!(history.already_processed(&feed, &item));

        let future = Utc::now() + chrono::Duration::days(2);
        history.purge_old(future);
        assert!(!history.already_processed(&feed, &item));
    }
}
