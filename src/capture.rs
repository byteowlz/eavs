//! Capture module for transparent traffic interception via mitmproxy.
//!
//! This module provides functionality to automatically start and manage
//! mitmproxy with the eavs_capture.py addon for transparent LLM API
//! traffic interception.

use crate::config::CaptureConfig;
use std::process::{Child, Command, Stdio};
use tokio::sync::oneshot;

/// Handle to a running mitmproxy capture process.
pub struct CaptureHandle {
    child: Child,
    _shutdown_tx: oneshot::Sender<()>,
}

impl CaptureHandle {
    /// Stop the mitmproxy process gracefully.
    pub fn stop(mut self) {
        tracing::info!("Stopping mitmproxy capture...");
        
        // Try graceful shutdown first (SIGTERM on Unix)
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
        
        // Wait for process to exit
        match self.child.wait() {
            Ok(status) => {
                tracing::info!("mitmproxy exited with status: {}", status);
            }
            Err(e) => {
                tracing::warn!("Failed to wait for mitmproxy: {}", e);
            }
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        // Ensure mitmproxy is killed when handle is dropped
        let _ = self.child.kill();
    }
}

/// Check if mitmproxy is installed and available.
pub fn check_mitmproxy_available(path: &str) -> Result<String, String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "mitmproxy not found at '{}'. Install with: brew install mitmproxy (macOS) or pip install mitmproxy",
                    path
                )
            } else {
                format!("Failed to run mitmproxy: {}", e)
            }
        })?;

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(version)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("mitmproxy check failed: {}", stderr))
    }
}

/// Start mitmproxy with the eavs_capture.py addon.
///
/// Returns a handle that can be used to stop the process, or an error
/// if mitmproxy couldn't be started.
pub fn start_capture(config: &CaptureConfig, eavs_port: u16) -> Result<CaptureHandle, String> {
    // Check if mitmproxy is available
    let version = check_mitmproxy_available(&config.mitmproxy_path)?;
    tracing::info!("Found mitmproxy: {}", version);

    // Check if addon script exists
    let addon_path = config.resolved_addon_path().ok_or_else(|| {
        "Could not find eavs_capture.py addon script. \
         Please set capture.addon_path in config or ensure the script is in the expected location."
            .to_string()
    })?;

    tracing::info!("Using capture addon: {}", addon_path.display());

    // Build mitmproxy arguments
    let args = config.build_mitmproxy_args(eavs_port);
    tracing::debug!("mitmproxy args: {:?}", args);

    // Spawn mitmproxy process
    let child = Command::new(&config.mitmproxy_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start mitmproxy: {}", e))?;

    let pid = child.id();
    tracing::info!(
        "Started mitmproxy capture (PID: {}) with mode '{}'",
        pid,
        config.mode
    );

    // Create shutdown channel (for future use with graceful shutdown)
    let (shutdown_tx, _shutdown_rx) = oneshot::channel();

    Ok(CaptureHandle {
        child,
        _shutdown_tx: shutdown_tx,
    })
}

/// Start capture mode asynchronously and log output.
pub async fn start_capture_async(
    config: CaptureConfig,
    eavs_port: u16,
) -> Result<CaptureHandle, String> {
    // Run the blocking check/start in a blocking task
    tokio::task::spawn_blocking(move || start_capture(&config, eavs_port))
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_mitmproxy_not_found() {
        let result = check_mitmproxy_available("/nonexistent/mitmproxy");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_capture_config_build_args() {
        let config = CaptureConfig {
            enabled: true,
            mitmproxy_path: "mitmproxy".to_string(),
            mode: "local:ChatGPT".to_string(),
            verbose: true,
            api_only: true,
            addon_path: Some("/path/to/addon.py".to_string()),
            extra_args: vec!["--quiet".to_string()],
        };

        let args = config.build_mitmproxy_args(3000);
        
        assert!(args.contains(&"--mode".to_string()));
        assert!(args.contains(&"local:ChatGPT".to_string()));
        assert!(args.contains(&"-s".to_string()));
        assert!(args.contains(&"eavs_port=3000".to_string()));
        assert!(args.contains(&"eavs_verbose=true".to_string()));
        assert!(args.contains(&"eavs_api_only=true".to_string()));
        assert!(args.contains(&"--quiet".to_string()));
    }
}
