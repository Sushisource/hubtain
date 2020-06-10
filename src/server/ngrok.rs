use anyhow::{anyhow, Error};
use async_std::task::spawn_blocking;
use serde::{Deserialize, Serialize};
use std::process::{Child, ExitStatus};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

/// Represents the json output from the ngrok client, specifically the line that we
/// care about parsing to get the URL to the tunnel.
///
/// EX:
/// ```json
/// {"addr":"//localhost:37465","lvl":"info","msg":"started tunnel",
///  "name":"command_line","obj":"tunnels",
///  "t":"2020-05-31T18:54:04.196782783-07:00",
///  "url":"tcp://0.tcp.ngrok.io:17585"}
/// ```
#[derive(Serialize, Deserialize)]
struct TunnelStartedMsg {
    addr: String,
    url: String,
    msg: String,
    lvl: String,
}

/// A handle to an ngrok process, which has already started and established a tunnel.
/// The handle must be told to shutdown when the server is done using it.
#[must_use = "ngrok handle must be held and told to shutdown"]
pub struct NgrokHandle {
    /// The external url parsed from the ngrok output
    url: String,
    /// The process handle
    handle: Child,
}

impl NgrokHandle {
    pub fn get_address(&self) -> &str {
        &self.url
    }

    pub fn shutdown(mut self) -> Result<ExitStatus, std::io::Error> {
        self.handle.kill()?;
        self.handle.wait()
    }
}

pub async fn get_tunnel(local_port: u16) -> Result<NgrokHandle, Error> {
    let mut handle = Command::new("ngrok")
        .args(&[
            "tcp",
            &local_port.to_string(),
            "--log",
            "stdout",
            "--log-format",
            "json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Watch for the output line indicating the tunnel URL
    let (tunnel, handle) = spawn_blocking(move || {
        let stdout = handle
            .stdout
            .as_mut()
            .ok_or_else(|| anyhow!("Couldn't obtain ngrok's stdout handle"))?;
        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            let maybestarted: Result<TunnelStartedMsg, _> = serde_json::from_str(&line?);
            if let Ok(m) = maybestarted {
                return Ok((m.url, handle));
            }
        }

        Err(anyhow!("Was unable to parse tunnel URL from ngrok output"))
    })
    .await?;

    Ok(NgrokHandle {
        url: tunnel,
        handle,
    })
}
