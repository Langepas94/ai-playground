use crate::{config::AppConfig, errors::AppError};

pub fn run_config_path() -> Result<(), AppError> {
    println!("{}", AppConfig::config_path()?.display());
    Ok(())
}
