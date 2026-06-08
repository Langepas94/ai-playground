use serde::{Serialize, de::DeserializeOwned};

use crate::errors::AppError;

pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, AppError> {
    let value = serde_json::to_value(value).map_err(|error| AppError::Json(error.to_string()))?;
    toon::to_string(&value).map_err(|error| AppError::Toon(error.to_string()))
}

pub fn from_str<T: DeserializeOwned>(raw: &str) -> Result<T, AppError> {
    toon::from_str(raw).map_err(|error| AppError::Toon(error.to_string()))
}

pub fn from_str_or_json<T: DeserializeOwned>(raw: &str) -> Result<T, AppError> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(raw)
            .map_err(|json_error| AppError::Json(json_error.to_string()))
            .or_else(|_| from_str(raw));
    }

    from_str(raw).or_else(|toon_error| {
        serde_json::from_str(raw).map_err(|json_error| match toon_error {
            AppError::Toon(toon_error) => {
                AppError::Toon(format!("{toon_error}; JSON fallback failed: {json_error}"))
            }
            other => other,
        })
    })
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Example {
        name: String,
        done: bool,
    }

    #[test]
    fn toon_roundtrip_works_for_structs() {
        let value = Example {
            name: "Ada".to_string(),
            done: true,
        };

        let raw = to_string(&value).expect("encode");
        assert!(raw.contains("name: Ada"));

        let decoded: Example = from_str(&raw).expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn json_fallback_keeps_legacy_inputs_readable() {
        let decoded: Example =
            from_str_or_json(r#"{"name":"Ada","done":true}"#).expect("decode json");

        assert_eq!(
            decoded,
            Example {
                name: "Ada".to_string(),
                done: true,
            }
        );
    }
}
