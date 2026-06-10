use std::io::{self, BufRead, Write};

use crate::errors::AppError;

pub(crate) fn prompt_required(label: &str) -> Result<String, AppError> {
    loop {
        let value = prompt(label)?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
        println!("Please enter a value.");
    }
}

pub(crate) fn prompt_with_default(label: &str, default: &str) -> Result<String, AppError> {
    let raw = prompt(&format!("{label} [{default}]"))?;
    if raw.trim().is_empty() {
        println!("Using default: {default}");
        Ok(default.to_string())
    } else {
        Ok(raw)
    }
}

pub(crate) fn prompt(label: &str) -> Result<String, AppError> {
    print!("{label}: ");
    io::stdout()
        .flush()
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    Ok(read_stdin_line()?.unwrap_or_default().trim().to_string())
}

pub(crate) fn read_stdin_line() -> Result<Option<String>, AppError> {
    let mut bytes = Vec::new();
    let read = io::stdin()
        .lock()
        .read_until(b'\n', &mut bytes)
        .map_err(|error| AppError::Terminal(error.to_string()))?;
    if read == 0 {
        return Ok(None);
    }
    while bytes
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        bytes.pop();
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

pub(crate) fn prompt_optional_secret(label: &str) -> Result<Option<String>, AppError> {
    let value = prompt(label)?;
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}
