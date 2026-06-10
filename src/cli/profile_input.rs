use crate::{
    config::{AppConfig, ProfileConfig},
    errors::AppError,
    providers::{ProviderClient, ProviderKind, ReqwestProviderClient, validate_base_url},
    secrets::{SecretStore, get_config_profile_token, set_profile_token},
};

use super::input::{prompt, prompt_optional_secret, prompt_required, prompt_with_default};

#[derive(Debug, Clone)]
pub struct CollectedProfile {
    pub name: String,
    pub config: ProfileConfig,
    pub token_saved: bool,
}

pub async fn collect_profile_input(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
) -> Result<CollectedProfile, AppError> {
    collect_profile_input_inner(name, provider, model, base_url, secrets, config, false).await
}

pub async fn collect_profile_input_with_optional_setup_token(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
) -> Result<CollectedProfile, AppError> {
    collect_profile_input_inner(name, provider, model, base_url, secrets, config, true).await
}

async fn collect_profile_input_inner(
    name: Option<String>,
    provider: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
    secrets: &dyn SecretStore,
    config: &AppConfig,
    prompt_setup_token: bool,
) -> Result<CollectedProfile, AppError> {
    let provider = match provider {
        Some(provider) => provider,
        None => prompt_provider()?,
    };
    let name = match name {
        Some(name) => name,
        None => prompt_with_default(
            "Profile name. This is just a local nickname for this provider",
            &provider.to_string(),
        )?,
    };
    let base_url = match base_url {
        Some(base_url) => base_url,
        None => prompt_with_default(
            "Base URL. Press Enter unless you use a custom compatible endpoint",
            provider.default_base_url(),
        )?,
    };
    let token_profile = ProfileConfig {
        provider,
        model: provider.default_model().to_string(),
        base_url: base_url.clone(),
        token_ref: crate::config::token_ref(&provider),
    };
    let setup_token = if prompt_setup_token {
        println!("The token will be stored in the OS keychain, not in config.");
        prompt_optional_secret("API token (paste it, or press Enter to skip)")?
    } else {
        None
    };
    if let Some(token) = setup_token.as_deref() {
        set_profile_token(secrets, &token_profile, token)?;
    }
    let token_saved = setup_token.is_some();
    let model_token = match setup_token {
        Some(token) => Some(token),
        None => get_config_profile_token(secrets, config, &name, &token_profile)?,
    };
    let model = match model {
        Some(model) => model,
        None => prompt_model(provider, &base_url, model_token.as_deref()).await?,
    };

    Ok(CollectedProfile {
        name,
        config: ProfileConfig {
            provider,
            model,
            base_url,
            token_ref: String::new(),
        },
        token_saved,
    })
}

pub async fn prompt_model(
    provider: ProviderKind,
    base_url: &str,
    token: Option<&str>,
) -> Result<String, AppError> {
    let spec = provider.spec();
    println!();
    println!("Choose model for {}.", spec.display_name);
    let models = match token {
        Some(token) => fetch_provider_models(provider, base_url, token).await?,
        None => Vec::new(),
    };
    if models.is_empty() {
        if token.is_none() {
            println!("No token is available yet, so the live model list cannot be loaded.");
        } else {
            println!("The live model list is empty; type a model id manually if needed.");
        }
        println!(
            "Press Enter for the provider default, or type a model id. Default: {}",
            spec.default_model
        );
    } else {
        println!("Loaded {} models from the provider.", models.len());
        println!(
            "Press Enter for the provider default, choose a number, or type a custom model id."
        );
        for (index, model) in models.iter().enumerate() {
            let recommended = if model == spec.default_model {
                " default"
            } else {
                ""
            };
            println!("  {}. {}{}", index + 1, model, recommended);
        }
        println!("  custom. Type another model id manually");
    }

    loop {
        let raw = prompt(&format!(
            "Model number or custom id [{}]",
            spec.default_model
        ))?;
        if raw.trim().is_empty() {
            return Ok(spec.default_model.to_string());
        }
        if let Ok(index) = raw.parse::<usize>()
            && let Some(model) = models.get(index.saturating_sub(1))
        {
            return Ok(model.to_string());
        }
        if raw.eq_ignore_ascii_case("custom") {
            return prompt_required("Custom model id");
        }
        if !raw.trim().is_empty() {
            return Ok(raw);
        }
        println!("Choose a model number, type custom, press Enter, or type a model id.");
    }
}

async fn fetch_provider_models(
    provider: ProviderKind,
    base_url: &str,
    token: &str,
) -> Result<Vec<String>, AppError> {
    validate_base_url(&provider.to_string(), base_url)?;
    let client = ReqwestProviderClient::new()?;
    let profile = ProfileConfig {
        provider,
        model: provider.default_model().to_string(),
        base_url: base_url.to_string(),
        token_ref: String::new(),
    };
    match client.list_models(&profile, token).await {
        Ok(models) => Ok(models),
        Err(error) => {
            eprintln!("Could not load live model list: {error}");
            Ok(Vec::new())
        }
    }
}

pub fn prompt_provider() -> Result<ProviderKind, AppError> {
    println!("Choose provider.");
    println!("If unsure, choose 2 for OpenRouter or press Enter for the recommended default.");
    for (index, provider) in ProviderKind::all().iter().enumerate() {
        let spec = provider.spec();
        let recommended = if *provider == ProviderKind::OpenRouter {
            " recommended"
        } else {
            ""
        };
        println!(
            "  {}. {} ({}) - default model: {}{}",
            index + 1,
            spec.display_name,
            provider,
            spec.default_model,
            recommended
        );
    }

    loop {
        let raw = prompt("Provider number or name [2]")?;
        if raw.trim().is_empty() {
            return Ok(ProviderKind::OpenRouter);
        }
        if let Ok(index) = raw.parse::<usize>()
            && let Some(provider) = ProviderKind::all().get(index.saturating_sub(1))
        {
            return Ok(*provider);
        }
        if let Ok(provider) = raw.parse::<ProviderKind>() {
            return Ok(provider);
        }
        println!("Unknown provider. Choose a number from the list or type a provider name.");
    }
}

pub fn select_profile_name(config: &AppConfig, parts: &[String]) -> Result<String, AppError> {
    if !parts.is_empty() {
        return Ok(profile_name_from_parts(parts));
    }
    if config.profiles.is_empty() {
        return Err(AppError::InvalidInput(
            "No profiles found. Run `ai setup` or `ai profile add` first.".to_string(),
        ));
    }

    println!("Choose active profile.");
    for (index, (name, profile)) in config.profiles.iter().enumerate() {
        let active = if config.active_profile.as_deref() == Some(name) {
            " active"
        } else {
            ""
        };
        println!(
            "  {}. {} [{}] {}{}",
            index + 1,
            name,
            profile.provider,
            profile.model,
            active
        );
    }

    loop {
        let label = match config.active_profile.as_deref() {
            Some(active) => format!("Profile number or name [{active}]"),
            None => "Profile number or name".to_string(),
        };
        let raw = prompt(&label)?;
        if raw.trim().is_empty()
            && let Some(active) = config.active_profile.as_deref()
        {
            return Ok(active.to_string());
        }
        if let Ok(index) = raw.parse::<usize>()
            && let Some(name) = config.profiles.keys().nth(index.saturating_sub(1))
        {
            return Ok(name.to_string());
        }
        if config.profiles.contains_key(raw.trim()) {
            return Ok(raw.trim().to_string());
        }
        println!("Choose a number from the list or type an existing profile name.");
    }
}

pub fn profile_name_from_parts(parts: &[String]) -> String {
    parts.join(" ")
}
