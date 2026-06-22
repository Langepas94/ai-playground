use crate::cli::args::McpToolsArgs;
use crate::config::{AppConfig, McpServerConfig};
use crate::errors::AppError;
use crate::mcp::connect_and_list;

/// Default configured server used when no `--server`, `--command`, or `--url`
/// is given.
const DEFAULT_SERVER: &str = "filesystem";

/// `ai mcp tools` — connect to an MCP server, run the initialize handshake,
/// and print the tools it exposes.
pub async fn run_mcp_tools(args: &McpToolsArgs) -> Result<(), AppError> {
    let (label, server) = resolve_server(args)?;

    println!("Connecting to {label}…");
    let connection = connect_and_list(&server).await?;

    let version = connection
        .server_version
        .as_deref()
        .map(|value| format!(" v{value}"))
        .unwrap_or_default();
    println!(
        "Connected to {}{version} ({} tools)",
        connection.server_name, connection.tool_count
    );

    for tool in &connection.tools {
        match &tool.description {
            Some(description) => println!("- {}: {}", tool.name, description),
            None => println!("- {}", tool.name),
        }
    }

    Ok(())
}

/// Decide which server to connect to. Precedence: explicit `--url`, then ad-hoc
/// `--command`, then a named `--server`, then the default configured server.
///
/// The config file is only loaded for the named-server branch so ad-hoc
/// `--command` / `--url` connections work even without a valid config.
fn resolve_server(args: &McpToolsArgs) -> Result<(String, McpServerConfig), AppError> {
    if let Some(url) = &args.url {
        return Ok((url.clone(), McpServerConfig::Http { url: url.clone() }));
    }

    if let Some(command) = &args.command {
        let label = format!("{command} {}", args.args.join(" "));
        return Ok((
            label.trim().to_string(),
            McpServerConfig::Stdio {
                command: command.clone(),
                args: args.args.clone(),
            },
        ));
    }

    let config = AppConfig::load()?;
    let name = args.server.as_deref().unwrap_or(DEFAULT_SERVER);
    let server = config.mcp_server(name)?.clone();
    Ok((name.to_string(), server))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(
        server: Option<&str>,
        command: Option<&str>,
        cmd_args: &[&str],
        url: Option<&str>,
    ) -> McpToolsArgs {
        McpToolsArgs {
            server: server.map(str::to_string),
            command: command.map(str::to_string),
            args: cmd_args.iter().map(|value| value.to_string()).collect(),
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn url_takes_precedence_over_command_and_server() {
        let (label, server) = resolve_server(&args(
            Some("configured"),
            Some("npx"),
            &["-y"],
            Some("https://host.test/mcp"),
        ))
        .expect("resolve");
        assert_eq!(label, "https://host.test/mcp");
        match server {
            McpServerConfig::Http { url } => assert_eq!(url, "https://host.test/mcp"),
            other => panic!("expected http server, got {other:?}"),
        }
    }

    #[test]
    fn command_takes_precedence_over_server_and_builds_label() {
        let (label, server) =
            resolve_server(&args(Some("configured"), Some("npx"), &["-y", "srv"], None))
                .expect("resolve");
        assert_eq!(label, "npx -y srv");
        match server {
            McpServerConfig::Stdio { command, args } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y".to_string(), "srv".to_string()]);
            }
            other => panic!("expected stdio server, got {other:?}"),
        }
    }
}
