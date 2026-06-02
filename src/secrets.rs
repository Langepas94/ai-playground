use std::{collections::HashMap, sync::Mutex};

use keyring::Entry;

use crate::errors::AppError;

const SERVICE: &str = "aiteach";

pub trait SecretStore: Send + Sync {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError>;
    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError>;
    fn delete_token(&self, token_ref: &str) -> Result<(), AppError>;
}

#[derive(Debug, Default)]
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError> {
        Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .set_password(token)
            .map_err(|error| AppError::Secret(error.to_string()))
    }

    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        match Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .get_password()
        {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(AppError::Secret(error.to_string())),
        }
    }

    fn delete_token(&self, token_ref: &str) -> Result<(), AppError> {
        match Entry::new(SERVICE, token_ref)
            .map_err(|error| AppError::Secret(error.to_string()))?
            .delete_credential()
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(AppError::Secret(error.to_string())),
        }
    }
}

#[derive(Debug, Default)]
pub struct MemorySecretStore {
    tokens: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn set_token(&self, token_ref: &str, token: &str) -> Result<(), AppError> {
        self.tokens
            .lock()
            .map_err(|error| AppError::Secret(error.to_string()))?
            .insert(token_ref.to_string(), token.to_string());
        Ok(())
    }

    fn get_token(&self, token_ref: &str) -> Result<Option<String>, AppError> {
        Ok(self
            .tokens
            .lock()
            .map_err(|error| AppError::Secret(error.to_string()))?
            .get(token_ref)
            .cloned())
    }

    fn delete_token(&self, token_ref: &str) -> Result<(), AppError> {
        self.tokens
            .lock()
            .map_err(|error| AppError::Secret(error.to_string()))?
            .remove(token_ref);
        Ok(())
    }
}

pub fn mask_token(token: &str) -> String {
    match token.len() {
        0 => "<empty>".to_string(),
        1..=8 => "****".to_string(),
        len => format!("{}...{}", &token[..4], &token[len - 4..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_masking_never_prints_full_secret() {
        assert_eq!(mask_token(""), "<empty>");
        assert_eq!(mask_token("abc"), "****");
        assert_eq!(mask_token("sk-1234567890"), "sk-1...7890");
    }
}
