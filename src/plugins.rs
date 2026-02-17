use crate::config::AnalysisPluginConfig;
use crate::state::{AppState, Injection};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PluginCommand {
    Inject {
        conversation_id: String,
        messages: Vec<Injection>,
    },
    Clear {
        conversation_id: String,
    },
}

pub fn start_analysis_plugins(state: AppState) -> Vec<tokio::task::JoinHandle<()>> {
    if !state.config.analysis.enabled {
        return Vec::new();
    }

    state
        .config
        .analysis
        .plugins
        .iter()
        .filter(|p| !p.command.trim().is_empty())
        .cloned()
        .map(|plugin| spawn_plugin(state.clone(), plugin))
        .collect()
}

fn spawn_plugin(state: AppState, plugin: AnalysisPluginConfig) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cmd = Command::new(&plugin.command);
        cmd.args(&plugin.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());

        for (key, value) in plugin.env {
            let resolved = resolve_env_value(&value);
            cmd.env(key, resolved);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(plugin = %plugin.name, command = %plugin.command, error = %e, "Failed to spawn analysis plugin");
                return;
            }
        };

        let mut stdin = match child.stdin.take() {
            Some(s) => s,
            None => return,
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => return,
        };

        let mut rx = state.analysis_tx.subscribe();
        let plugin_name = plugin.name.clone();

        let writer = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let Ok(line) = serde_json::to_string(&event) else {
                            continue;
                        };
                        if stdin.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                        if stdin.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let reader_state = state.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<PluginCommand>(&line) {
                    Ok(cmd) => {
                        if let Err(e) = handle_plugin_command(&reader_state, cmd).await {
                            tracing::warn!(plugin = %plugin_name, error = %e, "Plugin command failed");
                        }
                    }
                    Err(e) => {
                        tracing::debug!(plugin = %plugin_name, error = %e, line = %line, "Invalid plugin output");
                    }
                }
            }
        });

        let waiter = tokio::spawn(async move {
            let _ = child.wait().await;
        });

        let _ = tokio::join!(writer, reader, waiter);
    })
}

async fn handle_plugin_command(state: &AppState, cmd: PluginCommand) -> Result<(), String> {
    match cmd {
        PluginCommand::Inject {
            conversation_id,
            messages,
        } => {
            // Mirror the HTTP inject handler behavior: deliver to active WS sessions if present,
            // otherwise queue for the next HTTP request.
            let delivered = state
                .ws_sessions
                .deliver_injections(&conversation_id, messages.clone());
            if !delivered {
                state
                    .conversations
                    .add_injections(&conversation_id, messages);
            }
            Ok(())
        }
        PluginCommand::Clear { conversation_id } => {
            state.conversations.clear(&conversation_id);
            state.injections.remove(&conversation_id);
            Ok(())
        }
    }
}

fn resolve_env_value(value: &str) -> String {
    if let Some(var) = value.strip_prefix("env:") {
        std::env::var(var).unwrap_or_default()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ProviderConfig, TransformConfig};
    use crate::upstream::{Upstream, UpstreamRequest};
    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use http::{HeaderMap, StatusCode};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Clone)]
    struct NoopUpstream;

    impl Upstream for NoopUpstream {
        fn send<'a>(
            &'a self,
            _request: UpstreamRequest,
        ) -> futures::future::BoxFuture<'a, Result<crate::upstream::UpstreamResponse, std::io::Error>>
        {
            Box::pin(async move {
                Ok(crate::upstream::UpstreamResponse {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: stream::iter(vec![Ok(Bytes::from_static(b"{}"))]).boxed(),
                })
            })
        }
    }

    fn config() -> AppConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                base_url: "http://up/v1".to_string(),
                ..Default::default()
            },
        );

        AppConfig {
            server: Default::default(),
            providers,
            upstream: HashMap::new(),
            logging: Default::default(),
            analysis: Default::default(),
            policy: Default::default(),
            state: Default::default(),
            keys: Default::default(),
            capture: Default::default(),
            transform: TransformConfig::default(),
            network: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_handle_plugin_command_inject_delivers_to_ws_session() {
        let state = AppState::new_with_upstream(config(), Arc::new(NoopUpstream));
        let (_token, mut rx) = state.ws_sessions.register("conv1");

        handle_plugin_command(
            &state,
            PluginCommand::Inject {
                conversation_id: "conv1".to_string(),
                messages: vec![Injection {
                    role: "system".to_string(),
                    content: "hello".to_string(),
                }],
            },
        )
        .await
        .unwrap();

        let delivered = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered[0].content, "hello");
    }

    #[test]
    fn test_resolve_env_value() {
        std::env::remove_var("EAVS_TEST_PLUGIN_ENV");
        assert_eq!(resolve_env_value("env:EAVS_TEST_PLUGIN_ENV"), "");
        std::env::set_var("EAVS_TEST_PLUGIN_ENV", "x");
        assert_eq!(resolve_env_value("env:EAVS_TEST_PLUGIN_ENV"), "x");
    }
}
