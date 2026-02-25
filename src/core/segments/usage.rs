use super::{Segment, SegmentData};
use crate::config::{InputData, SegmentId};
use crate::utils::credentials;
use chrono::{DateTime, Datelike, Duration, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct ApiUsageResponse {
    five_hour: UsagePeriod,
    seven_day: UsagePeriod,
}

#[derive(Debug, Deserialize)]
struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiUsageCache {
    five_hour_utilization: f64,
    seven_day_utilization: f64,
    #[serde(default)]
    five_hour_resets_at: Option<String>,
    #[serde(default)]
    seven_day_resets_at: Option<String>,
    // Legacy cache field used before split reset timestamps.
    #[serde(default, rename = "resets_at", skip_serializing)]
    legacy_resets_at: Option<String>,
    cached_at: String,
}

#[derive(Default)]
pub struct UsageSegment;

#[derive(Debug, Clone, Copy, Default)]
enum ResetPeriod {
    #[default]
    Session,
    Weekly,
}

impl ResetPeriod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Weekly => "weekly",
        }
    }
}

impl TryFrom<&str> for ResetPeriod {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.eq_ignore_ascii_case("session") {
            Ok(Self::Session)
        } else if value.eq_ignore_ascii_case("weekly") {
            Ok(Self::Weekly)
        } else {
            Err(())
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
enum ResetFormat {
    #[default]
    Time,
    Duration,
}

impl ResetFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Duration => "duration",
        }
    }
}

impl TryFrom<&str> for ResetFormat {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.eq_ignore_ascii_case("time") {
            Ok(Self::Time)
        } else if value.eq_ignore_ascii_case("duration") {
            Ok(Self::Duration)
        } else {
            Err(())
        }
    }
}

impl UsageSegment {
    pub fn new() -> Self {
        Self
    }

    fn get_circle_icon(utilization: f64) -> String {
        let percent = (utilization * 100.0) as u8;
        match percent {
            0..=12 => "\u{f0a9e}".to_string(),  // circle_slice_1
            13..=25 => "\u{f0a9f}".to_string(), // circle_slice_2
            26..=37 => "\u{f0aa0}".to_string(), // circle_slice_3
            38..=50 => "\u{f0aa1}".to_string(), // circle_slice_4
            51..=62 => "\u{f0aa2}".to_string(), // circle_slice_5
            63..=75 => "\u{f0aa3}".to_string(), // circle_slice_6
            76..=87 => "\u{f0aa4}".to_string(), // circle_slice_7
            _ => "\u{f0aa5}".to_string(),       // circle_slice_8
        }
    }

    fn format_reset_time(reset_time_str: Option<&str>) -> String {
        if let Some(time_str) = reset_time_str {
            if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                let mut local_dt = dt.with_timezone(&Local);
                if local_dt.minute() > 45 {
                    local_dt += Duration::hours(1);
                }
                return format!(
                    "{}-{}-{}",
                    local_dt.month(),
                    local_dt.day(),
                    local_dt.hour()
                );
            }
        }
        "?".to_string()
    }

    fn format_reset_duration(reset_time_str: Option<&str>) -> String {
        if let Some(time_str) = reset_time_str {
            if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                let now = Utc::now();
                let reset_utc = dt.with_timezone(&Utc);
                let remaining = reset_utc.signed_duration_since(now);

                if remaining.num_seconds() < 60 {
                    return "now".to_string();
                }

                let total_minutes = remaining.num_minutes();
                let days = total_minutes / (24 * 60);
                let hours = (total_minutes % (24 * 60)) / 60;
                let minutes = total_minutes % 60;

                return if days > 0 {
                    format!("{}d {}h", days, hours)
                } else if hours > 0 {
                    format!("{}h {}m", hours, minutes)
                } else {
                    format!("{}m", minutes)
                };
            }
        }
        "?".to_string()
    }

    fn get_cache_path() -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Some(
            home.join(".claude")
                .join("ccline")
                .join(".api_usage_cache.json"),
        )
    }

    fn load_cache(&self) -> Option<ApiUsageCache> {
        let cache_path = Self::get_cache_path()?;
        if !cache_path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&cache_path).ok()?;
        let mut cache: ApiUsageCache = serde_json::from_str(&content).ok()?;

        if let Some(legacy_resets_at) = cache.legacy_resets_at.clone() {
            if cache.five_hour_resets_at.is_none() {
                cache.five_hour_resets_at = Some(legacy_resets_at.clone());
            }
            if cache.seven_day_resets_at.is_none() {
                cache.seven_day_resets_at = Some(legacy_resets_at);
            }
        }

        Some(cache)
    }

    fn save_cache(&self, cache: &ApiUsageCache) {
        if let Some(cache_path) = Self::get_cache_path() {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(cache) {
                let _ = std::fs::write(&cache_path, json);
            }
        }
    }

    fn is_cache_valid(&self, cache: &ApiUsageCache, cache_duration: u64) -> bool {
        if let Ok(cached_at) = DateTime::parse_from_rfc3339(&cache.cached_at) {
            let now = Utc::now();
            let elapsed = now.signed_duration_since(cached_at.with_timezone(&Utc));
            elapsed.num_seconds() < cache_duration as i64
        } else {
            false
        }
    }

    fn get_claude_code_version() -> String {
        use std::process::Command;

        let output = Command::new("npm")
            .args(["view", "@anthropic-ai/claude-code", "version"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !version.is_empty() {
                    return format!("claude-code/{}", version);
                }
            }
            _ => {}
        }

        "claude-code".to_string()
    }

    fn get_proxy_from_settings() -> Option<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let settings_path = format!("{}/.claude/settings.json", home);

        let content = std::fs::read_to_string(&settings_path).ok()?;
        let settings: serde_json::Value = serde_json::from_str(&content).ok()?;

        // Try HTTPS_PROXY first, then HTTP_PROXY
        settings
            .get("env")?
            .get("HTTPS_PROXY")
            .or_else(|| settings.get("env")?.get("HTTP_PROXY"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn fetch_api_usage(
        &self,
        api_base_url: &str,
        token: &str,
        timeout_secs: u64,
    ) -> Option<ApiUsageResponse> {
        let url = format!("{}/api/oauth/usage", api_base_url);
        let user_agent = Self::get_claude_code_version();

        let mut agent_builder = ureq::AgentBuilder::new();

        // Configure proxy from Claude settings if available
        if let Some(proxy_url) = Self::get_proxy_from_settings() {
            if let Ok(proxy) = ureq::Proxy::new(&proxy_url) {
                agent_builder = agent_builder.proxy(proxy);
            }
        }

        let agent = agent_builder.build();

        let response = agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", token))
            .set("anthropic-beta", "oauth-2025-04-20")
            .set("User-Agent", &user_agent)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .call()
            .ok()?;

        if response.status() == 200 {
            response.into_json().ok()
        } else {
            None
        }
    }
}

impl Segment for UsageSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        let token = credentials::get_oauth_token()?;

        // Load config from file to get segment options
        let config = crate::config::Config::load().ok()?;
        let segment_config = config.segments.iter().find(|s| s.id == SegmentId::Usage);

        let api_base_url = segment_config
            .and_then(|sc| sc.options.get("api_base_url"))
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.anthropic.com");

        let cache_duration = segment_config
            .and_then(|sc| sc.options.get("cache_duration"))
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        let timeout = segment_config
            .and_then(|sc| sc.options.get("timeout"))
            .and_then(|v| v.as_u64())
            .unwrap_or(2);

        let cached_data = self.load_cache();
        let use_cached = cached_data
            .as_ref()
            .map(|cache| self.is_cache_valid(cache, cache_duration))
            .unwrap_or(false);

        let reset_period_raw = segment_config
            .and_then(|sc| sc.options.get("reset_period"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let reset_period = reset_period_raw
            .as_deref()
            .and_then(|value| ResetPeriod::try_from(value).ok())
            .unwrap_or_default();

        let reset_format_raw = segment_config
            .and_then(|sc| sc.options.get("reset_format"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let reset_format = reset_format_raw
            .as_deref()
            .and_then(|value| ResetFormat::try_from(value).ok())
            .unwrap_or_default();

        let (five_hour_util, seven_day_util, five_hour_resets_at, seven_day_resets_at) =
            if use_cached {
                if let Some(cache) = cached_data.as_ref() {
                    (
                        cache.five_hour_utilization,
                        cache.seven_day_utilization,
                        cache.five_hour_resets_at.clone(),
                        cache.seven_day_resets_at.clone(),
                    )
                } else {
                    return None;
                }
            } else {
                match self.fetch_api_usage(api_base_url, &token, timeout) {
                    Some(response) => {
                        let cache = ApiUsageCache {
                            five_hour_utilization: response.five_hour.utilization,
                            seven_day_utilization: response.seven_day.utilization,
                            five_hour_resets_at: response.five_hour.resets_at.clone(),
                            seven_day_resets_at: response.seven_day.resets_at.clone(),
                            legacy_resets_at: None,
                            cached_at: Utc::now().to_rfc3339(),
                        };
                        self.save_cache(&cache);
                        (
                            response.five_hour.utilization,
                            response.seven_day.utilization,
                            response.five_hour.resets_at,
                            response.seven_day.resets_at,
                        )
                    }
                    None => {
                        if let Some(cache) = cached_data {
                            (
                                cache.five_hour_utilization,
                                cache.seven_day_utilization,
                                cache.five_hour_resets_at,
                                cache.seven_day_resets_at,
                            )
                        } else {
                            return None;
                        }
                    }
                }
            };

        let resets_at = match reset_period {
            ResetPeriod::Session => five_hour_resets_at
                .as_deref()
                .or(seven_day_resets_at.as_deref()),
            ResetPeriod::Weekly => seven_day_resets_at
                .as_deref()
                .or(five_hour_resets_at.as_deref()),
        };

        let dynamic_icon = Self::get_circle_icon(seven_day_util / 100.0);
        let five_hour_percent = five_hour_util.round() as u8;
        let primary = format!("{}%", five_hour_percent);
        let reset_str = match reset_format {
            ResetFormat::Duration => Self::format_reset_duration(resets_at),
            ResetFormat::Time => Self::format_reset_time(resets_at),
        };
        let secondary = format!("· {}", reset_str);

        let mut metadata = HashMap::new();
        metadata.insert("dynamic_icon".to_string(), dynamic_icon);
        metadata.insert(
            "five_hour_utilization".to_string(),
            five_hour_util.to_string(),
        );
        metadata.insert(
            "seven_day_utilization".to_string(),
            seven_day_util.to_string(),
        );
        metadata.insert(
            "reset_period".to_string(),
            reset_period.as_str().to_string(),
        );
        metadata.insert(
            "reset_format".to_string(),
            reset_format.as_str().to_string(),
        );
        if let Some(invalid_reset_period) = reset_period_raw
            .as_deref()
            .filter(|value| ResetPeriod::try_from(*value).is_err())
        {
            metadata.insert(
                "invalid_reset_period".to_string(),
                invalid_reset_period.to_string(),
            );
        }
        if let Some(invalid_reset_format) = reset_format_raw
            .as_deref()
            .filter(|value| ResetFormat::try_from(*value).is_err())
        {
            metadata.insert(
                "invalid_reset_format".to_string(),
                invalid_reset_format.to_string(),
            );
        }

        Some(SegmentData {
            primary,
            secondary,
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Usage
    }
}

#[cfg(test)]
mod tests {
    use super::{ResetFormat, ResetPeriod, UsageSegment};
    use chrono::{Duration, Utc};

    #[test]
    fn reset_period_parses_expected_values() {
        assert!(matches!(
            ResetPeriod::try_from("session"),
            Ok(ResetPeriod::Session)
        ));
        assert!(matches!(
            ResetPeriod::try_from("WEEKLY"),
            Ok(ResetPeriod::Weekly)
        ));
        assert!(ResetPeriod::try_from("invalid").is_err());
    }

    #[test]
    fn reset_format_parses_expected_values() {
        assert!(matches!(
            ResetFormat::try_from("time"),
            Ok(ResetFormat::Time)
        ));
        assert!(matches!(
            ResetFormat::try_from("DURATION"),
            Ok(ResetFormat::Duration)
        ));
        assert!(ResetFormat::try_from("invalid").is_err());
    }

    #[test]
    fn duration_under_one_minute_is_now() {
        let reset_at = (Utc::now() + Duration::seconds(30)).to_rfc3339();
        let display = UsageSegment::format_reset_duration(Some(&reset_at));

        assert_eq!(display, "now");
    }
}
