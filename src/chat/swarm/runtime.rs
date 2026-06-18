//! Runtime resolution of a [`SwarmConfig`] into per-role provider profiles and
//! tokens. This is where config/secrets are read; `ChatAgent` only ever sees the
//! already-resolved [`ResolvedSwarm`], so the "config and secrets stay separate"
//! invariant holds.

use std::str::FromStr;

use crate::{
    config::{AppConfig, ProfileConfig},
    providers::ProviderKind,
    secrets::{SecretStore, get_config_profile_token},
};

use super::config::{SubAgentConfig, SubAgentRole, SwarmConfig};

/// A fully-resolved sub-agent: provider profile + token ready for a sub-request.
#[derive(Debug, Clone)]
pub struct ResolvedSubAgent {
    pub role: SubAgentRole,
    /// Legacy compatibility mirror. Roles are mandatory and always resolved as
    /// enabled; the field remains for older debug/UI consumers.
    pub enabled: bool,
    pub profile: ProfileConfig,
    pub token: String,
    /// Custom system prompt; empty means "use the role default".
    pub system_prompt: String,
    /// True when this agent reuses the main agent's provider/model/token.
    pub inherits: bool,
}

/// All resolved sub-agents for one `ChatAgent`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedSwarm {
    agents: Vec<ResolvedSubAgent>,
}

impl ResolvedSwarm {
    /// Every role enabled and inheriting the main agent's profile + token. Used
    /// as `ChatAgent`'s default so the always-on Memory agent works even when a
    /// caller never sets an explicit swarm.
    pub fn inherit_all(main_profile: &ProfileConfig, main_token: &str) -> Self {
        let agents = SubAgentRole::ALL
            .iter()
            .map(|&role| ResolvedSubAgent {
                role,
                enabled: true,
                profile: main_profile.clone(),
                token: main_token.to_string(),
                system_prompt: String::new(),
                inherits: true,
            })
            .collect();
        Self { agents }
    }

    pub fn for_role(&self, role: SubAgentRole) -> Option<&ResolvedSubAgent> {
        self.agents.iter().find(|agent| agent.role == role)
    }

    pub fn is_enabled(&self, role: SubAgentRole) -> bool {
        let _ = role;
        true
    }

    pub fn agents(&self) -> &[ResolvedSubAgent] {
        &self.agents
    }
}

/// Resolve a persisted [`SwarmConfig`] against the main agent's profile/token,
/// the app config (for sibling-profile token lookup) and the secret store.
///
/// Inheritance rules per sub-agent:
/// - No provider/base_url override → reuse the main profile and token; only the
///   model may be swapped (same credentials).
/// - Provider/base_url override → build a profile from the override (filling
///   gaps with the provider defaults) and resolve a token via
///   [`get_config_profile_token`]; if no token is found, fall back to the main
///   token so the agent still runs (best effort) and mark it as inheriting.
pub fn resolve_swarm(
    main_profile: &ProfileConfig,
    main_token: &str,
    config: &SwarmConfig,
    app_config: &AppConfig,
    secrets: &dyn SecretStore,
) -> ResolvedSwarm {
    let mut agents = Vec::with_capacity(SubAgentRole::ALL.len());
    for role in SubAgentRole::ALL {
        let cfg = config.get(role);
        agents.push(resolve_one(
            &cfg,
            main_profile,
            main_token,
            app_config,
            secrets,
        ));
    }
    ResolvedSwarm { agents }
}

fn resolve_one(
    cfg: &SubAgentConfig,
    main_profile: &ProfileConfig,
    main_token: &str,
    app_config: &AppConfig,
    secrets: &dyn SecretStore,
) -> ResolvedSubAgent {
    let system_prompt = cfg.system_prompt.trim().to_string();

    // Common case: same provider/credentials, optional model swap.
    if cfg.inherits_provider() {
        let mut profile = main_profile.clone();
        let model = cfg.model.trim();
        if !model.is_empty() {
            profile.model = model.to_string();
        }
        let inherits = model.is_empty();
        return ResolvedSubAgent {
            role: cfg.role,
            enabled: true,
            profile,
            token: main_token.to_string(),
            system_prompt,
            inherits,
        };
    }

    // Custom provider/base_url: build a profile and resolve its token.
    let provider = ProviderKind::from_str(cfg.provider.trim()).unwrap_or(main_profile.provider);
    let base_url = if cfg.base_url.trim().is_empty() {
        if provider == main_profile.provider {
            main_profile.base_url.clone()
        } else {
            provider.default_base_url().to_string()
        }
    } else {
        cfg.base_url.trim().to_string()
    };
    let model = if cfg.model.trim().is_empty() {
        if provider == main_profile.provider {
            main_profile.model.clone()
        } else {
            provider.default_model().to_string()
        }
    } else {
        cfg.model.trim().to_string()
    };
    let profile = ProfileConfig {
        provider,
        model,
        base_url,
        token_ref: main_profile.token_ref.clone(),
    };

    let token = get_config_profile_token(secrets, app_config, "swarm-subagent", &profile)
        .ok()
        .flatten();
    match token {
        Some(token) => ResolvedSubAgent {
            role: cfg.role,
            enabled: true,
            profile,
            token,
            system_prompt,
            inherits: false,
        },
        // No token for the overridden provider: fall back to the main agent so
        // the sub-agent still runs instead of silently dying.
        None => ResolvedSubAgent {
            role: cfg.role,
            enabled: true,
            profile: main_profile.clone(),
            token: main_token.to_string(),
            system_prompt,
            inherits: true,
        },
    }
}
