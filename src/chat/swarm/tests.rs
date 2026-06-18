use super::*;
use crate::config::{AppConfig, ProfileConfig};
use crate::providers::ProviderKind;
use crate::secrets::MemorySecretStore;

fn main_profile() -> ProfileConfig {
    ProfileConfig {
        provider: ProviderKind::OpenAiCompatible,
        model: "main-model".to_string(),
        base_url: "https://main.example".to_string(),
        token_ref: "main-ref".to_string(),
    }
}

#[test]
fn defaults_enable_every_role() {
    let config = SwarmConfig::defaults();
    assert_eq!(config.agents.len(), SubAgentRole::ALL.len());
    for role in SubAgentRole::ALL {
        assert!(config.is_enabled(role), "{role} should default enabled");
        assert!(config.get(role).inherits_provider());
    }
}

#[test]
fn config_serde_round_trip() {
    let mut config = SwarmConfig::defaults();
    config.set(SubAgentConfig {
        role: SubAgentRole::Memory,
        enabled: true,
        provider: "deepseek".to_string(),
        base_url: String::new(),
        model: "deepseek-chat".to_string(),
        system_prompt: "extract facts".to_string(),
    });
    let json = serde_json::to_string(&config).expect("serialize");
    let parsed: SwarmConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, config);
    // Role serializes as a plain scalar string.
    assert!(json.contains("\"memory\""));
}

#[test]
fn missing_roles_are_filled_by_normalized() {
    let partial = SwarmConfig {
        agents: vec![SubAgentConfig::inherit(SubAgentRole::Memory)],
    };
    let full = partial.normalized();
    assert_eq!(full.agents.len(), SubAgentRole::ALL.len());
    for role in SubAgentRole::ALL {
        assert!(full.agents.iter().any(|agent| agent.role == role));
    }
}

#[test]
fn inherit_all_reuses_main_profile_and_token() {
    let resolved = ResolvedSwarm::inherit_all(&main_profile(), "main-token");
    for role in SubAgentRole::ALL {
        let agent = resolved.for_role(role).expect("role present");
        assert!(agent.inherits);
        assert_eq!(agent.token, "main-token");
        assert_eq!(agent.profile.model, "main-model");
    }
}

#[test]
fn model_only_override_keeps_main_token() {
    let mut config = SwarmConfig::defaults();
    config.set(SubAgentConfig {
        role: SubAgentRole::Summary,
        enabled: true,
        provider: String::new(),
        base_url: String::new(),
        model: "cheap-model".to_string(),
        system_prompt: String::new(),
    });
    let secrets = MemorySecretStore::default();
    let app = AppConfig::default();
    let resolved = resolve_swarm(&main_profile(), "main-token", &config, &app, &secrets);
    let summary = resolved.for_role(SubAgentRole::Summary).expect("summary");
    assert_eq!(summary.profile.model, "cheap-model");
    assert_eq!(summary.token, "main-token");
    assert_eq!(summary.profile.provider, ProviderKind::OpenAiCompatible);
    // Not flagged as full inherit because the model was swapped.
    assert!(!summary.inherits);
}

#[test]
fn persisted_disabled_role_is_forced_back_on() {
    let config = SwarmConfig {
        agents: vec![SubAgentConfig {
            role: SubAgentRole::Invariant,
            enabled: false,
            ..SubAgentConfig::inherit(SubAgentRole::Invariant)
        }],
    };
    let secrets = MemorySecretStore::default();
    let app = AppConfig::default();
    let resolved = resolve_swarm(&main_profile(), "main-token", &config, &app, &secrets);
    let invariant = resolved
        .for_role(SubAgentRole::Invariant)
        .expect("mandatory role present");
    assert!(invariant.enabled);
    assert!(resolved.is_enabled(SubAgentRole::Invariant));
}
