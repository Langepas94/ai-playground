use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use directories::ProjectDirs;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{
    errors::AppError,
    providers::{ModelPricing, ProviderKind},
};

pub const LITELLM_PRICE_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const PRICE_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PricingResolution {
    pub pricing: ModelPricing,
    pub source: PriceSource,
    pub source_url: String,
    pub fetched_at_unix: u64,
    pub stale: bool,
    pub matched_model: String,
    pub context_length: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PriceSource {
    LiteLlmCatalog,
    OfficialProviderDocs,
    ProviderModelsApi,
    ManualOverride,
}

impl std::fmt::Display for PriceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LiteLlmCatalog => write!(f, "litellm-catalog"),
            Self::OfficialProviderDocs => write!(f, "official-provider-docs"),
            Self::ProviderModelsApi => write!(f, "provider-models-api"),
            Self::ManualOverride => write!(f, "manual-override"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceCatalogStatus {
    pub path: PathBuf,
    pub exists: bool,
    pub fetched_at_unix: Option<u64>,
    pub stale: bool,
    pub source_url: String,
    pub model_count: usize,
}

#[derive(Debug, Clone)]
pub struct LiteLlmPriceCatalog {
    path: PathBuf,
    ttl: Duration,
}

impl LiteLlmPriceCatalog {
    pub fn new() -> Result<Self, AppError> {
        Ok(Self {
            path: default_cache_path()?,
            ttl: PRICE_CACHE_TTL,
        })
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            ttl: PRICE_CACHE_TTL,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn status(&self) -> Result<PriceCatalogStatus, AppError> {
        let cache = self.load_cache().ok();
        Ok(PriceCatalogStatus {
            path: self.path.clone(),
            exists: self.path.exists(),
            fetched_at_unix: cache.as_ref().map(|cache| cache.fetched_at_unix),
            stale: cache
                .as_ref()
                .map(|cache| is_stale(cache.fetched_at_unix, self.ttl))
                .unwrap_or(true),
            source_url: LITELLM_PRICE_CATALOG_URL.to_string(),
            model_count: cache.map(|cache| cache.entries.len()).unwrap_or_default(),
        })
    }

    pub async fn sync(&self, client: &Client) -> Result<PriceCatalogStatus, AppError> {
        let raw = client
            .get(LITELLM_PRICE_CATALOG_URL)
            .send()
            .await
            .map_err(AppError::from)?
            .error_for_status()
            .map_err(AppError::from)?
            .text()
            .await
            .map_err(AppError::from)?;
        let entries = parse_litellm_entries(&raw)?;
        let cache = PriceCache {
            fetched_at_unix: unix_seconds(),
            source_url: LITELLM_PRICE_CATALOG_URL.to_string(),
            entries,
        };
        self.save_cache(&cache)?;
        self.status()
    }

    pub async fn sync_if_stale(&self, client: &Client) -> Result<PriceCatalogStatus, AppError> {
        let status = self.status()?;
        if status.stale {
            return self.sync(client).await;
        }
        Ok(status)
    }

    pub fn resolve(
        &self,
        provider: ProviderKind,
        model: &str,
    ) -> Result<Option<PricingResolution>, AppError> {
        let cache = match self.load_cache() {
            Ok(cache) => cache,
            Err(error) => {
                if let Some(resolution) =
                    official_provider_resolution(provider, model, unix_seconds(), false)
                {
                    return Ok(Some(resolution));
                }
                return Err(error);
            }
        };
        Ok(resolve_from_cache(&cache, provider, model, self.ttl))
    }

    fn load_cache(&self) -> Result<PriceCache, AppError> {
        let raw = fs::read_to_string(&self.path).map_err(|error| AppError::Config {
            path: self.path.clone(),
            message: format!("could not read price cache: {error}"),
        })?;
        serde_json::from_str(&raw).map_err(|error| AppError::Config {
            path: self.path.clone(),
            message: format!("invalid price cache JSON: {error}"),
        })
    }

    fn save_cache(&self, cache: &PriceCache) -> Result<(), AppError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| AppError::Config {
                path: parent.to_path_buf(),
                message: format!("could not create price cache directory: {error}"),
            })?;
        }
        let raw = serde_json::to_string_pretty(cache)
            .map_err(|error| AppError::Json(error.to_string()))?;
        fs::write(&self.path, raw).map_err(|error| AppError::Config {
            path: self.path.clone(),
            message: format!("could not write price cache: {error}"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriceCache {
    fetched_at_unix: u64,
    source_url: String,
    entries: BTreeMap<String, LiteLlmModelEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LiteLlmModelEntry {
    litellm_provider: Option<String>,
    input_cost_per_token: Option<serde_json::Value>,
    output_cost_per_token: Option<serde_json::Value>,
    cache_read_input_token_cost: Option<serde_json::Value>,
    cache_creation_input_token_cost: Option<serde_json::Value>,
    max_input_tokens: Option<serde_json::Value>,
    max_tokens: Option<serde_json::Value>,
    source: Option<String>,
    mode: Option<String>,
}

fn default_cache_path() -> Result<PathBuf, AppError> {
    let dirs = ProjectDirs::from("dev", "ai-playground", "ai-playground").ok_or_else(|| {
        AppError::Config {
            path: PathBuf::from("<unknown>"),
            message: "Could not resolve OS data directory".to_string(),
        }
    })?;
    Ok(dirs
        .data_local_dir()
        .join("pricing")
        .join("litellm_prices.json"))
}

fn parse_litellm_entries(raw: &str) -> Result<BTreeMap<String, LiteLlmModelEntry>, AppError> {
    serde_json::from_str(raw)
        .map_err(|error| AppError::Json(format!("invalid LiteLLM price catalog: {error}")))
}

fn resolve_from_cache(
    cache: &PriceCache,
    provider: ProviderKind,
    model: &str,
    ttl: Duration,
) -> Option<PricingResolution> {
    let model = model.trim();
    if let Some(resolution) = official_provider_resolution(
        provider,
        model,
        cache.fetched_at_unix,
        is_stale(cache.fetched_at_unix, ttl),
    ) {
        return Some(resolution);
    }
    let candidates = model_candidates(provider, model);
    for candidate in &candidates {
        if let Some(entry) = cache.entries.get(candidate.as_str()) {
            if !provider_matches(provider, entry) {
                continue;
            }
            return resolution_from_entry(cache, candidate, entry, ttl);
        }
    }
    let model_names = normalized_model_names(model);
    for (candidate, entry) in &cache.entries {
        if !provider_matches(provider, entry) {
            continue;
        }
        if !model_names
            .iter()
            .any(|name| catalog_key_matches_model(candidate, name))
        {
            continue;
        }
        return resolution_from_entry(cache, candidate, entry, ttl);
    }
    None
}

fn official_provider_resolution(
    provider: ProviderKind,
    model: &str,
    fetched_at_unix: u64,
    stale: bool,
) -> Option<PricingResolution> {
    match provider {
        ProviderKind::DeepSeek => official_deepseek_resolution(model, fetched_at_unix, stale),
        _ => None,
    }
}

fn official_deepseek_resolution(
    model: &str,
    fetched_at_unix: u64,
    stale: bool,
) -> Option<PricingResolution> {
    let normalized = model.trim().to_ascii_lowercase();
    let bare = normalized.rsplit('/').next().unwrap_or(&normalized);
    let tier = match bare {
        "deepseek-v4-flash"
        | "deepseek-v4-flash-thinking"
        | "deepseek-chat"
        | "deepseek-reasoner" => Some(("deepseek-v4-flash", 0.14, 0.28, 0.0028)),
        "deepseek-v4-pro" | "deepseek-v4-pro-thinking" | "deepseek-pro" => {
            Some(("deepseek-v4-pro", 0.435, 0.87, 0.003625))
        }
        _ => None,
    }?;
    Some(PricingResolution {
        pricing: ModelPricing {
            currency: "USD".to_string(),
            input_per_million: Some(tier.1),
            output_per_million: tier.2,
            cache_hit_input_per_million: Some(tier.3),
            cache_miss_input_per_million: Some(tier.1),
        },
        source: PriceSource::OfficialProviderDocs,
        source_url: "https://api-docs.deepseek.com/quick_start/pricing".to_string(),
        fetched_at_unix,
        stale,
        matched_model: tier.0.to_string(),
        context_length: Some(1_000_000),
    })
}

fn resolution_from_entry(
    cache: &PriceCache,
    matched_model: &str,
    entry: &LiteLlmModelEntry,
    ttl: Duration,
) -> Option<PricingResolution> {
    let pricing = pricing_from_entry(entry)?;
    Some(PricingResolution {
        pricing,
        source: PriceSource::LiteLlmCatalog,
        source_url: entry
            .source
            .clone()
            .unwrap_or_else(|| cache.source_url.clone()),
        fetched_at_unix: cache.fetched_at_unix,
        stale: is_stale(cache.fetched_at_unix, ttl),
        matched_model: matched_model.to_string(),
        context_length: entry
            .max_input_tokens
            .as_ref()
            .and_then(parse_u64_value)
            .or_else(|| entry.max_tokens.as_ref().and_then(parse_u64_value)),
    })
}

fn pricing_from_entry(entry: &LiteLlmModelEntry) -> Option<ModelPricing> {
    let output = entry
        .output_cost_per_token
        .as_ref()
        .and_then(parse_f64_value)?;
    Some(ModelPricing {
        currency: "USD".to_string(),
        input_per_million: entry
            .input_cost_per_token
            .as_ref()
            .and_then(parse_f64_value)
            .map(per_token_to_per_million),
        output_per_million: per_token_to_per_million(output),
        cache_hit_input_per_million: entry
            .cache_read_input_token_cost
            .as_ref()
            .and_then(parse_f64_value)
            .map(per_token_to_per_million),
        cache_miss_input_per_million: entry
            .cache_creation_input_token_cost
            .as_ref()
            .and_then(parse_f64_value)
            .map(per_token_to_per_million),
    })
}

fn parse_f64_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    }
}

fn parse_u64_value(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    }
}

fn per_token_to_per_million(value: f64) -> f64 {
    value * 1_000_000.0
}

fn model_candidates(provider: ProviderKind, model: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for name in normalized_model_names(model) {
        candidates.push(name.clone());
        for prefix in provider_prefixes(provider) {
            candidates.push(format!("{prefix}/{name}"));
        }
        if provider == ProviderKind::OpenAiCompatible {
            candidates.push(format!("openai/{name}"));
        }
    }
    dedupe(candidates)
}

fn normalized_model_names(model: &str) -> Vec<String> {
    let model = model.trim();
    let mut values = vec![model.to_string()];
    if let Some((_, bare)) = model.rsplit_once('/') {
        values.push(bare.to_string());
    }
    values.push(model.to_ascii_lowercase());
    if let Some((_, bare)) = model.rsplit_once('/') {
        values.push(bare.to_ascii_lowercase());
    }
    dedupe(values)
}

fn provider_prefixes(provider: ProviderKind) -> &'static [&'static str] {
    match provider {
        ProviderKind::OpenAiCompatible => &["openai"],
        ProviderKind::OpenRouter => &[],
        ProviderKind::DeepSeek => &[
            "deepseek",
            "deepseek-ai",
            "deepseek-chat",
            "deepseek-reasoner",
        ],
        ProviderKind::GigaChat => &["gigachat"],
        ProviderKind::Kimi => &["moonshot", "kimi"],
    }
}

fn provider_matches(provider: ProviderKind, entry: &LiteLlmModelEntry) -> bool {
    if provider == ProviderKind::OpenRouter {
        return true;
    }
    let Some(entry_provider) = entry.litellm_provider.as_deref() else {
        return true;
    };
    if provider == ProviderKind::DeepSeek
        && entry_provider.to_ascii_lowercase().starts_with("deepseek")
    {
        return true;
    }
    provider_prefixes(provider)
        .iter()
        .any(|candidate| *candidate == entry_provider)
        || (provider == ProviderKind::OpenAiCompatible && entry_provider == "openai")
}

fn catalog_key_matches_model(catalog_key: &str, model_name: &str) -> bool {
    catalog_key == model_name
        || catalog_key
            .rsplit_once('/')
            .map(|(_, bare)| bare == model_name)
            .unwrap_or(false)
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn is_stale(fetched_at_unix: u64, ttl: Duration) -> bool {
    unix_seconds().saturating_sub(fetched_at_unix) > ttl.as_secs()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(entries: BTreeMap<String, LiteLlmModelEntry>) -> PriceCache {
        PriceCache {
            fetched_at_unix: unix_seconds(),
            source_url: LITELLM_PRICE_CATALOG_URL.to_string(),
            entries,
        }
    }

    fn entry(provider: &str, input: f64, output: f64) -> LiteLlmModelEntry {
        LiteLlmModelEntry {
            litellm_provider: Some(provider.to_string()),
            input_cost_per_token: Some(serde_json::json!(input)),
            output_cost_per_token: Some(serde_json::json!(output)),
            cache_read_input_token_cost: Some(serde_json::json!(input / 2.0)),
            cache_creation_input_token_cost: None,
            max_input_tokens: Some(serde_json::json!(128_000)),
            max_tokens: Some(serde_json::json!(16_000)),
            source: Some("https://example.test/pricing".to_string()),
            mode: Some("chat".to_string()),
        }
    }

    #[test]
    fn resolves_exact_direct_provider_price() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "deepseek-chat".to_string(),
            entry("deepseek", 0.00000028, 0.00000042),
        );

        let resolved = resolve_from_cache(
            &cache(entries),
            ProviderKind::DeepSeek,
            "deepseek-chat",
            PRICE_CACHE_TTL,
        )
        .expect("price");

        assert_eq!(resolved.source, PriceSource::OfficialProviderDocs);
        assert_eq!(resolved.matched_model, "deepseek-v4-flash");
        assert_eq!(resolved.context_length, Some(1_000_000));
        assert!((resolved.pricing.input_per_million.unwrap() - 0.14).abs() < f64::EPSILON);
        assert!((resolved.pricing.output_per_million - 0.28).abs() < f64::EPSILON);
    }

    #[test]
    fn resolves_deepseek_prices_with_catalog_provider_aliases() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "deepseek-ai/deepseek-coder".to_string(),
            entry("deepseek-ai", 0.00000028, 0.00000042),
        );
        entries.insert(
            "deepseek-special".to_string(),
            entry("deepseek-special", 0.00000055, 0.00000219),
        );

        let chat = resolve_from_cache(
            &cache(entries.clone()),
            ProviderKind::DeepSeek,
            "deepseek-coder",
            PRICE_CACHE_TTL,
        )
        .expect("deepseek coder price");
        assert_eq!(chat.matched_model, "deepseek-ai/deepseek-coder");
        assert!((chat.pricing.output_per_million - 0.42).abs() < f64::EPSILON);

        let reasoner = resolve_from_cache(
            &cache(entries),
            ProviderKind::DeepSeek,
            "deepseek-special",
            PRICE_CACHE_TTL,
        )
        .expect("deepseek special price");
        assert_eq!(reasoner.matched_model, "deepseek-special");
        assert!((reasoner.pricing.output_per_million - 2.19).abs() < 1e-12);
    }

    #[test]
    fn resolves_deepseek_v4_official_prices_without_cache_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = LiteLlmPriceCatalog::with_path(dir.path().join("missing.json"));

        let pro = catalog
            .resolve(ProviderKind::DeepSeek, "deepseek-v4-pro")
            .expect("resolve")
            .expect("official deepseek pro pricing");
        assert_eq!(pro.source, PriceSource::OfficialProviderDocs);
        assert_eq!(pro.matched_model, "deepseek-v4-pro");
        assert_eq!(pro.context_length, Some(1_000_000));
        assert!((pro.pricing.input_per_million.unwrap() - 0.435).abs() < f64::EPSILON);
        assert!((pro.pricing.output_per_million - 0.87).abs() < f64::EPSILON);
        assert!((pro.pricing.cache_hit_input_per_million.unwrap() - 0.003625).abs() < f64::EPSILON);

        let flash = catalog
            .resolve(ProviderKind::DeepSeek, "deepseek-v4-flash")
            .expect("resolve")
            .expect("official deepseek flash pricing");
        assert_eq!(flash.matched_model, "deepseek-v4-flash");
        assert_eq!(flash.context_length, Some(1_000_000));
        assert!((flash.pricing.input_per_million.unwrap() - 0.14).abs() < f64::EPSILON);
        assert!((flash.pricing.output_per_million - 0.28).abs() < f64::EPSILON);
    }

    #[test]
    fn official_deepseek_aliases_override_stale_catalog_values() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "deepseek-chat".to_string(),
            entry("deepseek", 0.00000028, 0.00000042),
        );

        let resolved = resolve_from_cache(
            &cache(entries),
            ProviderKind::DeepSeek,
            "deepseek-chat",
            PRICE_CACHE_TTL,
        )
        .expect("official alias price");

        assert_eq!(resolved.source, PriceSource::OfficialProviderDocs);
        assert_eq!(resolved.matched_model, "deepseek-v4-flash");
        assert_eq!(resolved.context_length, Some(1_000_000));
        assert!((resolved.pricing.input_per_million.unwrap() - 0.14).abs() < f64::EPSILON);
        assert!((resolved.pricing.output_per_million - 0.28).abs() < f64::EPSILON);
    }

    #[test]
    fn resolves_provider_prefixed_model() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "gigachat/GigaChat-2-Pro".to_string(),
            entry("gigachat", 0.0, 0.0),
        );

        let resolved = resolve_from_cache(
            &cache(entries),
            ProviderKind::GigaChat,
            "GigaChat-2-Pro",
            PRICE_CACHE_TTL,
        )
        .expect("price");

        assert_eq!(resolved.matched_model, "gigachat/GigaChat-2-Pro");
    }

    #[test]
    fn resolves_openrouter_prices_for_any_underlying_provider() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "anthropic/claude-3-5-sonnet".to_string(),
            entry("anthropic", 0.000003, 0.000015),
        );

        let resolved = resolve_from_cache(
            &cache(entries),
            ProviderKind::OpenRouter,
            "anthropic/claude-3-5-sonnet",
            PRICE_CACHE_TTL,
        )
        .expect("openrouter price");

        assert_eq!(resolved.matched_model, "anthropic/claude-3-5-sonnet");
        assert!((resolved.pricing.input_per_million.unwrap() - 3.0).abs() < f64::EPSILON);
        assert!((resolved.pricing.output_per_million - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn resolves_prices_when_provider_returns_prefixed_model_but_catalog_has_bare_key() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "moonshot-v1-8k".to_string(),
            entry("moonshot", 0.000001, 0.000001),
        );

        let resolved = resolve_from_cache(
            &cache(entries),
            ProviderKind::Kimi,
            "moonshot/moonshot-v1-8k",
            PRICE_CACHE_TTL,
        )
        .expect("kimi price");

        assert_eq!(resolved.matched_model, "moonshot-v1-8k");
    }

    #[test]
    fn does_not_cross_provider_match() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "deepseek-chat".to_string(),
            entry("deepseek", 0.00000028, 0.00000042),
        );

        let resolved = resolve_from_cache(
            &cache(entries),
            ProviderKind::Kimi,
            "deepseek-chat",
            PRICE_CACHE_TTL,
        );

        assert!(resolved.is_none());
    }
}
