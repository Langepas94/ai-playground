use crate::{
    dist::{default_config, install, status, suggested_install_dir},
    errors::AppError,
};

use super::super::args::DistInstallArgs;

pub async fn run_dist_install(args: &DistInstallArgs, overwrite: bool) -> Result<(), AppError> {
    let config = default_config(
        args.channel.clone(),
        args.install_dir
            .clone()
            .or_else(|| Some(suggested_install_dir())),
        args.url.clone(),
    )?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(AppError::from)?;
    let result = install(&client, &config, overwrite || args.overwrite).await?;
    println!(
        "Installed dev build to {}\nsource: {}\nbytes: {}",
        result.install_path.display(),
        result.download_url,
        result.bytes_written
    );
    Ok(())
}

pub async fn run_dist_update(args: &DistInstallArgs) -> Result<(), AppError> {
    run_dist_install(args, true).await
}

pub fn run_dist_status(args: &DistInstallArgs) -> Result<(), AppError> {
    let config = default_config(
        args.channel.clone(),
        args.install_dir
            .clone()
            .or_else(|| Some(suggested_install_dir())),
        args.url.clone(),
    )?;
    let st = status(&config);
    println!(
        "install_path: {}\ndownload_url: {}\ninstalled: {}\nexecutable: {}",
        st.install_path.display(),
        st.download_url,
        st.installed,
        st.executable
    );
    println!(
        "hint: add {} to PATH or run it directly",
        st.install_path.display()
    );
    Ok(())
}
