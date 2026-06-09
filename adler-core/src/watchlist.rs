//! Watchlist configuration model.
//!
//! This module is intentionally runtime-free: it models who should be
//! watched and which registry scope each watched identity uses. CLI,
//! Web, MCP, or a future scheduler can parse JSON/TOML/YAML into these
//! serde-compatible structs and then call [`WatchlistConfig::scan_targets`]
//! to get concrete `(username, SiteFilter)` work items.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{SiteFilter, Username};

/// Current schema version for watchlist configuration documents.
pub const WATCHLIST_CONFIG_SCHEMA_VERSION: u16 = 1;

/// Top-level watchlist configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchlistConfig {
    /// Schema version for tolerant future readers.
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    /// Optional default scan scope inherited by every target.
    #[serde(default)]
    pub default_scope: WatchScope,
    /// Optional repeated-scan policy. Runtime surfaces decide how to execute it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScanSchedule>,
    /// Watched identities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<WatchTarget>,
}

impl Default for WatchlistConfig {
    fn default() -> Self {
        Self {
            schema_version: WATCHLIST_CONFIG_SCHEMA_VERSION,
            default_scope: WatchScope::default(),
            schedule: None,
            targets: Vec::new(),
        }
    }
}

impl WatchlistConfig {
    /// Validate targets, aliases, and duplicate scan usernames.
    ///
    /// The same concrete username appearing twice is rejected even if it
    /// arrives through aliases, because a later timeline cannot safely decide
    /// which watched identity owns that scan artifact.
    pub fn validate(&self) -> Result<(), WatchlistError> {
        if let Some(schedule) = &self.schedule {
            schedule.validate()?;
        }

        let mut seen = HashSet::new();
        for (index, target) in self.targets.iter().enumerate() {
            if target.username.trim().is_empty() {
                return Err(WatchlistError::EmptyUsername {
                    target_index: index,
                });
            }
            validate_username(&target.username)?;
            insert_unique(&mut seen, &target.username)?;
            for alias in &target.aliases {
                if alias.trim().is_empty() {
                    return Err(WatchlistError::EmptyAlias {
                        username: target.username.clone(),
                    });
                }
                validate_username(alias)?;
                insert_unique(&mut seen, alias)?;
            }
        }
        Ok(())
    }

    /// Expand watched identities into concrete scan targets.
    ///
    /// Each target yields one scan for its primary username and one scan per
    /// alias. The returned scope is the merged default + per-target scope.
    pub fn scan_targets(&self) -> Result<Vec<WatchScanTarget>, WatchlistError> {
        self.validate()?;
        let mut out = Vec::new();
        for target in &self.targets {
            let scope = self.default_scope.merged(&target.scope).to_site_filter();
            out.push(WatchScanTarget {
                identity: target.username.clone(),
                username: target.username.clone(),
                scope: scope.clone(),
            });
            for alias in &target.aliases {
                out.push(WatchScanTarget {
                    identity: target.username.clone(),
                    username: alias.clone(),
                    scope: scope.clone(),
                });
            }
        }
        Ok(out)
    }
}

const fn default_schema_version() -> u16 {
    WATCHLIST_CONFIG_SCHEMA_VERSION
}

/// Repeated scan policy for a watchlist.
///
/// This type intentionally contains no timers, tasks, or async runtime hooks.
/// A caller can persist the last started scan timestamp, ask whether a plan is
/// due, and then launch scans using its own scheduler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSchedule {
    /// Seconds between repeated scans. Must be greater than zero.
    pub every_secs: u64,
    /// Optional Unix epoch millisecond timestamp before which the plan is not
    /// due. Omit for an immediately due first scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at_ms: Option<u64>,
}

impl ScanSchedule {
    /// Validate interval bounds.
    pub fn validate(&self) -> Result<(), WatchlistError> {
        if self.every_secs == 0 {
            return Err(WatchlistError::InvalidSchedule {
                reason: "every_secs must be greater than zero".to_owned(),
            });
        }
        if self.every_secs > u64::MAX / 1_000 {
            return Err(WatchlistError::InvalidSchedule {
                reason: "every_secs is too large to convert to milliseconds".to_owned(),
            });
        }
        Ok(())
    }

    /// Millisecond timestamp when the next scan is due.
    ///
    /// `last_started_at_ms` is the timestamp of the previous scan start for
    /// this schedule. `None` means the first scan has not run yet.
    #[must_use]
    pub fn next_due_ms(&self, last_started_at_ms: Option<u64>) -> u64 {
        let interval_ms = self.every_secs.saturating_mul(1_000);
        let due_after_last = last_started_at_ms.map(|last| last.saturating_add(interval_ms));
        match (due_after_last, self.start_at_ms) {
            (Some(due), Some(start_at)) => due.max(start_at),
            (Some(due), None) => due,
            (None, Some(start_at)) => start_at,
            (None, None) => 0,
        }
    }

    /// Whether the schedule is due at `now_ms`.
    #[must_use]
    pub fn is_due(&self, last_started_at_ms: Option<u64>, now_ms: u64) -> bool {
        self.next_due_ms(last_started_at_ms) <= now_ms
    }
}

/// One watched identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchTarget {
    /// Primary username / handle.
    pub username: String,
    /// Additional handles that should be tracked as the same identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Optional scope overriding or extending the watchlist default.
    #[serde(default)]
    pub scope: WatchScope,
}

/// Site/tag scope for watchlist scans.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchScope {
    /// Keep only sites whose name contains at least one term.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
    /// Drop sites whose name contains any term.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Keep only sites carrying at least one requested tag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag: Vec<String>,
    /// Drop sites carrying any of these tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_tag: Vec<String>,
    /// Include `nsfw`-tagged sites. `None` means inherit the default scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_nsfw: Option<bool>,
    /// Optional popularity-rank ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<u32>,
}

impl WatchScope {
    /// Merge a default scope with an overriding per-target scope.
    ///
    /// List fields are appended so a target can narrow a default scope with
    /// extra include/exclude terms. Scalar fields override only when set.
    #[must_use]
    pub fn merged(&self, override_scope: &Self) -> Self {
        let mut merged = self.clone();
        merged.only.extend(override_scope.only.clone());
        merged.exclude.extend(override_scope.exclude.clone());
        merged.tag.extend(override_scope.tag.clone());
        merged
            .exclude_tag
            .extend(override_scope.exclude_tag.clone());
        if override_scope.include_nsfw.is_some() {
            merged.include_nsfw = override_scope.include_nsfw;
        }
        if override_scope.top.is_some() {
            merged.top = override_scope.top;
        }
        merged
    }

    /// Convert into the core registry filter.
    #[must_use]
    pub fn to_site_filter(&self) -> SiteFilter {
        SiteFilter {
            include: self.only.clone(),
            exclude: self.exclude.clone(),
            tags: self.tag.clone(),
            exclude_tags: self.exclude_tag.clone(),
            include_nsfw: self.include_nsfw.unwrap_or(false),
            top: self.top,
        }
    }
}

/// Concrete scan work item derived from a watchlist.
#[derive(Debug, Clone)]
pub struct WatchScanTarget {
    /// Primary watched identity this scan contributes to.
    pub identity: String,
    /// Username/alias to scan.
    pub username: String,
    /// Registry scope for this scan.
    pub scope: SiteFilter,
}

/// Watchlist validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchlistError {
    /// Target had an empty username.
    #[error("watch target at index {target_index} has an empty username")]
    EmptyUsername {
        /// Index in [`WatchlistConfig::targets`].
        target_index: usize,
    },
    /// Alias was empty.
    #[error("watch target {username:?} has an empty alias")]
    EmptyAlias {
        /// Primary username carrying the empty alias.
        username: String,
    },
    /// Username failed core validation.
    #[error("invalid username {username:?}: {reason}")]
    InvalidUsername {
        /// Username or alias that failed validation.
        username: String,
        /// Human-readable validation reason.
        reason: String,
    },
    /// Same concrete username appeared more than once.
    #[error("duplicate watch username or alias {username:?}")]
    DuplicateUsername {
        /// Duplicated username/alias.
        username: String,
    },
    /// Schedule policy is invalid.
    #[error("invalid watch schedule: {reason}")]
    InvalidSchedule {
        /// Human-readable validation reason.
        reason: String,
    },
}

fn validate_username(username: &str) -> Result<(), WatchlistError> {
    Username::new(username.to_owned()).map_err(|err| WatchlistError::InvalidUsername {
        username: username.to_owned(),
        reason: err.to_string(),
    })?;
    Ok(())
}

fn insert_unique(seen: &mut HashSet<String>, username: &str) -> Result<(), WatchlistError> {
    let key = username.to_ascii_lowercase();
    if !seen.insert(key) {
        return Err(WatchlistError::DuplicateUsername {
            username: username.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_schema_version_and_empty_targets() {
        let cfg: WatchlistConfig = serde_json::from_str("{}").unwrap();

        assert_eq!(cfg.schema_version, WATCHLIST_CONFIG_SCHEMA_VERSION);
        assert!(cfg.targets.is_empty());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn serializes_compact_scope() {
        let cfg = WatchlistConfig {
            default_scope: WatchScope {
                tag: vec!["social".into()],
                exclude_tag: vec!["bot-protected".into()],
                top: Some(100),
                ..WatchScope::default()
            },
            schedule: Some(ScanSchedule {
                every_secs: 86_400,
                start_at_ms: Some(1_800_000_000_000),
            }),
            targets: vec![WatchTarget {
                username: "alice".into(),
                aliases: vec!["alice_dev".into()],
                scope: WatchScope::default(),
            }],
            ..WatchlistConfig::default()
        };

        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["schema_version"], WATCHLIST_CONFIG_SCHEMA_VERSION);
        assert_eq!(json["default_scope"]["tag"][0], "social");
        assert_eq!(json["schedule"]["every_secs"], 86_400);
        assert_eq!(json["targets"][0]["username"], "alice");
        assert_eq!(json["targets"][0]["aliases"][0], "alice_dev");
        assert!(json["targets"][0].get("scope").is_some());
    }

    #[test]
    fn schedule_is_due_immediately_without_start_or_previous_run() {
        let schedule = ScanSchedule {
            every_secs: 60,
            start_at_ms: None,
        };

        assert_eq!(schedule.next_due_ms(None), 0);
        assert!(schedule.is_due(None, 1));
    }

    #[test]
    fn schedule_uses_start_and_last_run_for_next_due() {
        let schedule = ScanSchedule {
            every_secs: 60,
            start_at_ms: Some(10_000),
        };

        assert_eq!(schedule.next_due_ms(None), 10_000);
        assert_eq!(schedule.next_due_ms(Some(12_000)), 72_000);
        assert!(!schedule.is_due(Some(12_000), 71_999));
        assert!(schedule.is_due(Some(12_000), 72_000));
    }

    #[test]
    fn validate_rejects_zero_schedule_interval() {
        let cfg = WatchlistConfig {
            schedule: Some(ScanSchedule {
                every_secs: 0,
                start_at_ms: None,
            }),
            ..WatchlistConfig::default()
        };

        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, WatchlistError::InvalidSchedule { .. }));
    }

    #[test]
    fn expands_aliases_with_merged_scope() {
        let cfg = WatchlistConfig {
            default_scope: WatchScope {
                tag: vec!["social".into()],
                exclude_tag: vec!["bot-protected".into()],
                top: Some(500),
                ..WatchScope::default()
            },
            targets: vec![WatchTarget {
                username: "alice".into(),
                aliases: vec!["alice_dev".into(), "alice-osint".into()],
                scope: WatchScope {
                    only: vec!["Git".into()],
                    tag: vec!["dev".into()],
                    top: Some(50),
                    ..WatchScope::default()
                },
            }],
            ..WatchlistConfig::default()
        };

        let targets = cfg.scan_targets().unwrap();

        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].identity, "alice");
        assert_eq!(targets[1].username, "alice_dev");
        assert_eq!(targets[2].username, "alice-osint");
        assert_eq!(targets[0].scope.include, ["Git"]);
        assert_eq!(targets[0].scope.tags, ["social", "dev"]);
        assert_eq!(targets[0].scope.exclude_tags, ["bot-protected"]);
        assert_eq!(targets[0].scope.top, Some(50));
    }

    #[test]
    fn rejects_duplicate_aliases_case_insensitively() {
        let cfg = WatchlistConfig {
            targets: vec![WatchTarget {
                username: "alice".into(),
                aliases: vec!["Alice".into()],
                scope: WatchScope::default(),
            }],
            ..WatchlistConfig::default()
        };

        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, WatchlistError::DuplicateUsername { .. }));
    }

    #[test]
    fn rejects_invalid_alias_username() {
        let cfg = WatchlistConfig {
            targets: vec![WatchTarget {
                username: "alice".into(),
                aliases: vec!["bad space".into()],
                scope: WatchScope::default(),
            }],
            ..WatchlistConfig::default()
        };

        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, WatchlistError::InvalidUsername { .. }));
    }
}
