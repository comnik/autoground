use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::record::TokenUsage;

#[derive(Debug, Clone, Deserialize)]
pub struct Prices {
    pub priced_at: String,
    pub source: String,
    pub rate_band: String,
    models: BTreeMap<String, ModelPrice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelPrice {
    pub input_usd_per_million: f64,
    pub cached_input_usd_per_million: f64,
    pub output_usd_per_million: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostEstimate {
    pub usd: Option<f64>,
    pub partial: bool,
}

impl Prices {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let text = fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Grok Build records `grok-4.7` sessions as `grok-4.7-build`. That suffix
    /// is not its own published SKU, so it uses the base model's list rates.
    fn price(&self, model: &str) -> Option<&ModelPrice> {
        self.models.get(model).or_else(|| {
            model
                .strip_suffix("-build")
                .and_then(|base| self.models.get(base))
        })
    }

    pub fn estimate(&self, model: Option<&str>, usage: &TokenUsage) -> CostEstimate {
        let Some(name) = model else {
            return CostEstimate {
                usd: None,
                partial: true,
            };
        };
        let Some(price) = self.price(name) else {
            return CostEstimate {
                usd: None,
                partial: true,
            };
        };
        let usd = usage.input_tokens as f64 * price.input_usd_per_million / 1_000_000.0
            + usage.cache_read_input_tokens as f64 * price.cached_input_usd_per_million
                / 1_000_000.0
            + usage.output_tokens as f64 * price.output_usd_per_million / 1_000_000.0;
        CostEstimate {
            usd: Some(usd),
            partial: usage.cache_creation_input_tokens > 0,
        }
    }
}

pub fn usage_from_headless(value: &Value) -> TokenUsage {
    let usage = value.get("usage").unwrap_or(value);
    TokenUsage {
        input_tokens: u64_field(usage, "input_tokens"),
        cache_read_input_tokens: u64_field(usage, "cache_read_input_tokens"),
        cache_creation_input_tokens: u64_field(usage, "cache_creation_input_tokens"),
        output_tokens: u64_field(usage, "output_tokens"),
        reasoning_tokens: u64_field(usage, "reasoning_tokens"),
    }
}

pub fn model_from_headless(value: &Value) -> Option<String> {
    if let Some(name) = value.get("model").and_then(Value::as_str) {
        if !name.is_empty() && name != "unknown" {
            return Some(name.to_string());
        }
    }
    let obj = value.get("modelUsage")?.as_object()?;
    if obj.len() == 1 {
        return obj.keys().next().cloned();
    }
    obj.iter()
        .max_by_key(|(_, row)| u64_field(row, "inputTokens"))
        .map(|(name, _)| name.clone())
}

pub fn server_cost_from_headless(value: &Value) -> Option<f64> {
    if let Some(cost) = value.get("total_cost_usd").and_then(Value::as_f64) {
        return Some(cost);
    }
    let ticks = value.get("total_cost_usd_ticks")?.as_f64()?;
    Some(ticks / 10_000_000_000.0)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price() -> ModelPrice {
        ModelPrice {
            input_usd_per_million: 2.0,
            cached_input_usd_per_million: 0.5,
            output_usd_per_million: 6.0,
        }
    }

    fn table() -> Prices {
        let mut models = BTreeMap::new();
        models.insert("grok-4.7".to_string(), price());
        Prices {
            priced_at: "2026-09-22".to_string(),
            source: "test".to_string(),
            rate_band: "under_200k".to_string(),
            models,
        }
    }

    fn usage() -> TokenUsage {
        TokenUsage {
            input_tokens: 1_000_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            output_tokens: 500_000,
            reasoning_tokens: 10_000,
        }
    }

    #[test]
    fn priced_tokens_match_the_short_context_rates() {
        let estimate = table().estimate(Some("grok-4.7"), &usage());
        assert!(!estimate.partial);
        assert!((estimate.usd.unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn cache_creation_is_left_out_and_marks_the_estimate_partial() {
        let mut tokens = usage();
        tokens.cache_creation_input_tokens = 1_000;
        let estimate = table().estimate(Some("grok-4.7"), &tokens);
        assert!(estimate.partial);
        assert!((estimate.usd.unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_build_suffix_uses_the_published_model_rate() {
        let estimate = table().estimate(Some("grok-4.7-build"), &usage());
        assert!(!estimate.partial);
        assert!((estimate.usd.unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn an_unknown_model_has_no_dollar_figure() {
        let estimate = table().estimate(Some("grok-missing"), &usage());
        assert!(estimate.usd.is_none());
        assert!(estimate.partial);
    }

    #[test]
    fn shipped_table_pins_grok_4_7() {
        let prices: Prices = toml::from_str(include_str!("prices.toml")).unwrap();
        let price = prices.models.get("grok-4.7").unwrap();
        assert_eq!(price.input_usd_per_million, 2.0);
        assert_eq!(price.cached_input_usd_per_million, 0.5);
        assert_eq!(price.output_usd_per_million, 6.0);
        assert_eq!(prices.priced_at, "2026-09-22");
    }
}
