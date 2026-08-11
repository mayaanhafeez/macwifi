use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

pub const RESULT_SCHEMA_VERSION: u8 = 1;

/// A speed-test backend. Apple works without installing any extra software.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SpeedtestProvider {
    #[default]
    Apple,
    Ookla,
    Netflix,
    Custom,
}

impl std::fmt::Display for SpeedtestProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Apple => "apple",
            Self::Ookla => "ookla",
            Self::Netflix => "netflix",
            Self::Custom => "custom",
        };
        f.write_str(name)
    }
}

impl SpeedtestProvider {
    /// Provider-aware process ceiling used by the CLI when none is configured.
    pub fn default_timeout(self) -> Duration {
        match self {
            Self::Apple => Duration::from_secs(30),
            Self::Ookla | Self::Netflix | Self::Custom => Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpeedtestOptions {
    pub provider: SpeedtestProvider,
    pub timeout: Duration,
    pub server_id: Option<u64>,
    pub custom_command: Option<PathBuf>,
    pub custom_args: Vec<String>,
}

impl Default for SpeedtestOptions {
    fn default() -> Self {
        Self {
            provider: SpeedtestProvider::Apple,
            timeout: SpeedtestProvider::Apple.default_timeout(),
            server_id: None,
            custom_command: None,
            custom_args: Vec::new(),
        }
    }
}

/// Provider-independent output for CLI JSON consumers and Rust front ends.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpeedtestResult {
    pub schema_version: u8,
    pub provider: SpeedtestProvider,
    pub download_mbps: Option<f64>,
    pub upload_mbps: Option<f64>,
    pub ping_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packet_loss_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responsiveness_rpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_downloaded: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_uploaded: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<SpeedtestServer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpeedtestServer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Streaming lifecycle events for terminals, JSON Lines consumers, and UIs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpeedtestEvent {
    Started {
        schema_version: u8,
        provider: SpeedtestProvider,
    },
    Progress {
        schema_version: u8,
        provider: SpeedtestProvider,
        elapsed_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        download_mbps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        upload_mbps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ping_ms: Option<f64>,
    },
    Complete {
        schema_version: u8,
        result: Box<SpeedtestResult>,
    },
    Failed {
        schema_version: u8,
        provider: SpeedtestProvider,
        elapsed_ms: u64,
        error: String,
    },
}

/// Run a speed test and normalize the provider's output into the public schema.
pub async fn run(options: &SpeedtestOptions) -> Result<SpeedtestResult> {
    run_with_progress(options, |_| {}).await
}

/// Run a speed test while reporting provider-independent lifecycle events.
pub async fn run_with_progress(
    options: &SpeedtestOptions,
    mut on_event: impl FnMut(SpeedtestEvent),
) -> Result<SpeedtestResult> {
    let started = Instant::now();
    on_event(SpeedtestEvent::Started {
        schema_version: RESULT_SCHEMA_VERSION,
        provider: options.provider,
    });

    let result = run_inner(options, started, &mut on_event).await;
    match result {
        Ok(result) => {
            on_event(SpeedtestEvent::Complete {
                schema_version: RESULT_SCHEMA_VERSION,
                result: Box::new(result.clone()),
            });
            Ok(result)
        }
        Err(error) => {
            on_event(SpeedtestEvent::Failed {
                schema_version: RESULT_SCHEMA_VERSION,
                provider: options.provider,
                elapsed_ms: elapsed_ms(started),
                error: format!("{error:#}"),
            });
            Err(error)
        }
    }
}

async fn run_inner(
    options: &SpeedtestOptions,
    started: Instant,
    on_event: &mut impl FnMut(SpeedtestEvent),
) -> Result<SpeedtestResult> {
    if options.timeout.is_zero() {
        bail!("speed-test timeout must be greater than zero");
    }
    if options.server_id.is_some() && !matches!(options.provider, SpeedtestProvider::Ookla) {
        bail!("--server-id is only supported by the Ookla provider");
    }

    let (program, args) = provider_command(options)?;
    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.process_group(0);
    if matches!(options.provider, SpeedtestProvider::Netflix)
        && std::env::var_os("PUPPETEER_EXECUTABLE_PATH").is_none()
    {
        let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        if Path::new(chrome).is_file() {
            command.env("PUPPETEER_EXECUTABLE_PATH", chrome);
        }
    }

    // networkQuality's -M limit covers measurement, then it needs a moment to
    // aggregate and serialize its computer-readable result.
    let process_timeout = if matches!(options.provider, SpeedtestProvider::Apple) {
        options.timeout.saturating_add(Duration::from_secs(5))
    } else {
        options.timeout
    };
    let child = command
        .spawn()
        .with_context(|| install_hint(options.provider, &program))?;
    let process_group = child.id().map(ProcessGroupGuard::new);
    let output_future = child.wait_with_output();
    tokio::pin!(output_future);
    let deadline = tokio::time::sleep(process_timeout);
    tokio::pin!(deadline);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let first_update = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut updates = tokio::time::interval_at(first_update, Duration::from_millis(500));
    updates.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (sampling_task, live_metrics) = SamplingTask::start();

    let output = loop {
        tokio::select! {
            output = &mut output_future => {
                break output.with_context(|| install_hint(options.provider, &program))?;
            }
            _ = &mut deadline => {
                if let Some(group) = &process_group {
                    group.kill();
                }
                bail!(
                    "{} speed test timed out after {} seconds",
                    options.provider,
                    options.timeout.as_secs()
                );
            }
            signal = &mut ctrl_c => {
                if let Some(group) = &process_group {
                    group.kill();
                }
                signal.context("listen for Ctrl-C")?;
                bail!("{} speed test cancelled", options.provider);
            }
            _ = updates.tick() => {
                let live = *live_metrics.borrow();
                on_event(SpeedtestEvent::Progress {
                    schema_version: RESULT_SCHEMA_VERSION,
                    provider: options.provider,
                    elapsed_ms: elapsed_ms(started),
                    download_mbps: live.download_mbps,
                    upload_mbps: live.upload_mbps,
                    ping_ms: live.ping_ms,
                });
            }
        }
    };
    if let Some(group) = &process_group {
        group.disarm();
    }
    drop(sampling_task);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            bail!(
                "{} speed test exited with {}",
                options.provider,
                output.status
            );
        }
        bail!("{} speed test failed: {stderr}", options.provider);
    }

    let stdout = std::str::from_utf8(&output.stdout)
        .with_context(|| format!("{} returned non-UTF-8 output", options.provider))?;
    let mut result = parse_output(options.provider, stdout)?;
    result.duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok(result)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

struct ProcessGroupGuard(std::sync::atomic::AtomicI32);

impl ProcessGroupGuard {
    fn new(pid: u32) -> Self {
        Self(std::sync::atomic::AtomicI32::new(pid as i32))
    }

    fn kill(&self) {
        let pid = self.0.load(std::sync::atomic::Ordering::Relaxed);
        if pid > 0 {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }

    fn disarm(&self) {
        self.0.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

struct SamplingTask(tokio::task::JoinHandle<()>);

impl SamplingTask {
    fn start() -> (Self, tokio::sync::watch::Receiver<LiveMetrics>) {
        let (tx, rx) = tokio::sync::watch::channel(LiveMetrics::default());
        let handle = tokio::spawn(async move {
            let Some(mut sampler) = LiveSampler::detect().await else {
                return;
            };
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                tx.send_replace(sampler.sample().await);
                if tx.is_closed() {
                    return;
                }
            }
        });
        (Self(handle), rx)
    }
}

impl Drop for SamplingTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone, Copy, Default)]
struct LiveMetrics {
    download_mbps: Option<f64>,
    upload_mbps: Option<f64>,
    ping_ms: Option<f64>,
}

struct LiveSampler {
    interface: String,
    previous: Option<InterfaceCounters>,
    ping_ms: Option<f64>,
    last_ping: Option<Instant>,
}

#[derive(Clone, Copy)]
struct InterfaceCounters {
    sampled_at: Instant,
    inbound_bytes: u64,
    outbound_bytes: u64,
}

impl LiveSampler {
    async fn detect() -> Option<Self> {
        let output = short_command_output(
            "/sbin/route",
            &["-n", "get", "default"],
            Duration::from_secs(2),
        )
        .await?;
        let text = std::str::from_utf8(&output.stdout).ok()?;
        let interface = parse_default_interface(text)?;
        let previous = read_interface_counters(&interface).await;
        Some(Self {
            interface,
            previous,
            ping_ms: None,
            last_ping: None,
        })
    }

    async fn sample(&mut self) -> LiveMetrics {
        let current = read_interface_counters(&self.interface).await;
        let (download_mbps, upload_mbps) = match (self.previous, current) {
            (Some(previous), Some(current)) => {
                let seconds = current
                    .sampled_at
                    .duration_since(previous.sampled_at)
                    .as_secs_f64();
                let download = current
                    .inbound_bytes
                    .checked_sub(previous.inbound_bytes)
                    .map(|bytes| bytes as f64 * 8.0 / seconds / 1_000_000.0);
                let upload = current
                    .outbound_bytes
                    .checked_sub(previous.outbound_bytes)
                    .map(|bytes| bytes as f64 * 8.0 / seconds / 1_000_000.0);
                (download, upload)
            }
            _ => (None, None),
        };
        self.previous = current;

        if self
            .last_ping
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
        {
            self.last_ping = Some(Instant::now());
            if let Some(ping_ms) = measure_ping().await {
                self.ping_ms = Some(ping_ms);
            }
        }

        LiveMetrics {
            download_mbps,
            upload_mbps,
            ping_ms: self.ping_ms,
        }
    }
}

async fn read_interface_counters(interface: &str) -> Option<InterfaceCounters> {
    let output = short_command_output(
        "/usr/sbin/netstat",
        &["-bI", interface],
        Duration::from_secs(2),
    )
    .await?;
    let text = std::str::from_utf8(&output.stdout).ok()?;
    let (inbound_bytes, outbound_bytes) = parse_interface_counters(text, interface)?;
    Some(InterfaceCounters {
        sampled_at: Instant::now(),
        inbound_bytes,
        outbound_bytes,
    })
}

async fn measure_ping() -> Option<f64> {
    let output = short_command_output(
        "/sbin/ping",
        &["-n", "-c", "1", "-W", "1000", "1.1.1.1"],
        Duration::from_millis(1500),
    )
    .await?;
    parse_ping(std::str::from_utf8(&output.stdout).ok()?)
}

async fn short_command_output(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .ok()?
        .ok()
}

fn parse_default_interface(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("interface:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parse_interface_counters(output: &str, interface: &str) -> Option<(u64, u64)> {
    output.lines().find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first().copied() != Some(interface) || fields.len() < 10 {
            return None;
        }
        Some((fields[6].parse().ok()?, fields[9].parse().ok()?))
    })
}

fn parse_ping(output: &str) -> Option<f64> {
    let start = output.find("time=")? + "time=".len();
    let end = output[start..].find(" ms")? + start;
    output[start..end].trim().parse().ok()
}

fn provider_command(options: &SpeedtestOptions) -> Result<(PathBuf, Vec<String>)> {
    let command = match options.provider {
        SpeedtestProvider::Apple => {
            let seconds = options.timeout.as_secs().max(1).to_string();
            (
                PathBuf::from("/usr/bin/networkQuality"),
                vec!["-c".into(), "-M".into(), seconds],
            )
        }
        SpeedtestProvider::Ookla => {
            let mut args = vec![
                "--format=json".into(),
                "--accept-license".into(),
                "--accept-gdpr".into(),
            ];
            if let Some(server_id) = options.server_id {
                args.push(format!("--server-id={server_id}"));
            }
            (PathBuf::from("speedtest"), args)
        }
        SpeedtestProvider::Netflix => (
            PathBuf::from("fast"),
            vec!["--upload".into(), "--json".into()],
        ),
        SpeedtestProvider::Custom => {
            let command = options.custom_command.clone().context(
                "the custom provider requires --custom-command or speedtest.custom_command",
            )?;
            (command, options.custom_args.clone())
        }
    };
    Ok(command)
}

fn install_hint(provider: SpeedtestProvider, program: &Path) -> String {
    match provider {
        SpeedtestProvider::Ookla => {
            "could not run `speedtest`; install the official Ookla Speedtest CLI".into()
        }
        SpeedtestProvider::Netflix => {
            "could not run `fast`; install it with `npm install --global fast-cli`".into()
        }
        _ => format!("could not run `{}`", program.display()),
    }
}

fn parse_output(provider: SpeedtestProvider, output: &str) -> Result<SpeedtestResult> {
    match provider {
        SpeedtestProvider::Apple => parse_apple(output),
        SpeedtestProvider::Ookla => parse_ookla(output),
        SpeedtestProvider::Netflix => parse_netflix(output),
        SpeedtestProvider::Custom => parse_custom(output),
    }
    .with_context(|| format!("parse {provider} speed-test output"))
}

fn empty_result(provider: SpeedtestProvider) -> SpeedtestResult {
    SpeedtestResult {
        schema_version: RESULT_SCHEMA_VERSION,
        provider,
        download_mbps: None,
        upload_mbps: None,
        ping_ms: None,
        loaded_latency_ms: None,
        jitter_ms: None,
        packet_loss_percent: None,
        responsiveness_rpm: None,
        bytes_downloaded: None,
        bytes_uploaded: None,
        interface: None,
        server: None,
        result_url: None,
        timestamp: None,
        duration_ms: 0,
    }
}

fn parse_apple(output: &str) -> Result<SpeedtestResult> {
    let value: Value = serde_json::from_str(output)?;
    let mut result = empty_result(SpeedtestProvider::Apple);
    result.download_mbps = number(&value, "/dl_throughput").map(|bps| bps / 1_000_000.0);
    result.upload_mbps = number(&value, "/ul_throughput").map(|bps| bps / 1_000_000.0);
    result.ping_ms = number(&value, "/base_rtt");
    result.responsiveness_rpm = number(&value, "/responsiveness");
    result.bytes_downloaded = integer(&value, "/dl_bytes_transferred");
    result.bytes_uploaded = integer(&value, "/ul_bytes_transferred");
    result.interface = string(&value, "/interface_name");
    result.timestamp = string(&value, "/end_date");
    result.server = string(&value, "/test_endpoint").map(|host| SpeedtestServer {
        id: None,
        name: None,
        location: None,
        host: Some(host),
    });
    require_primary_metric(result)
}

fn parse_ookla(output: &str) -> Result<SpeedtestResult> {
    let value: Value = serde_json::from_str(output)?;
    let mut result = empty_result(SpeedtestProvider::Ookla);
    result.download_mbps = number(&value, "/download/bandwidth").map(|v| v * 8.0 / 1_000_000.0);
    result.upload_mbps = number(&value, "/upload/bandwidth").map(|v| v * 8.0 / 1_000_000.0);
    result.ping_ms = number(&value, "/ping/latency");
    result.jitter_ms = number(&value, "/ping/jitter");
    result.packet_loss_percent = number(&value, "/packetLoss");
    result.bytes_downloaded = integer(&value, "/download/bytes");
    result.bytes_uploaded = integer(&value, "/upload/bytes");
    result.interface = string(&value, "/interface/name");
    result.result_url = string(&value, "/result/url");
    result.timestamp = string(&value, "/timestamp");
    result.server = Some(SpeedtestServer {
        id: integer(&value, "/server/id"),
        name: string(&value, "/server/name"),
        location: string(&value, "/server/location"),
        host: string(&value, "/server/host"),
    });
    require_primary_metric(result)
}

fn parse_netflix(output: &str) -> Result<SpeedtestResult> {
    let value: Value = serde_json::from_str(output)?;
    let mut result = empty_result(SpeedtestProvider::Netflix);
    result.download_mbps = number(&value, "/downloadSpeed");
    result.upload_mbps = number(&value, "/uploadSpeed");
    result.ping_ms = number(&value, "/latency");
    result.loaded_latency_ms = number(&value, "/bufferBloat");
    result.server = string(&value, "/userLocation").map(|location| SpeedtestServer {
        id: None,
        name: Some("fast.com".into()),
        location: Some(location),
        host: Some("fast.com".into()),
    });
    require_primary_metric(result)
}

fn parse_custom(output: &str) -> Result<SpeedtestResult> {
    let value: Value = serde_json::from_str(output)?;
    let mut result = empty_result(SpeedtestProvider::Custom);
    result.download_mbps = number(&value, "/download_mbps");
    result.upload_mbps = number(&value, "/upload_mbps");
    result.ping_ms = number(&value, "/ping_ms");
    result.loaded_latency_ms = number(&value, "/loaded_latency_ms");
    result.jitter_ms = number(&value, "/jitter_ms");
    result.packet_loss_percent = number(&value, "/packet_loss_percent");
    result.bytes_downloaded = integer(&value, "/bytes_downloaded");
    result.bytes_uploaded = integer(&value, "/bytes_uploaded");
    result.interface = string(&value, "/interface");
    result.result_url = string(&value, "/result_url");
    result.timestamp = string(&value, "/timestamp");
    if let Some(server) = value.get("server") {
        result.server = Some(serde_json::from_value(server.clone())?);
    }
    require_primary_metric(result)
}

fn require_primary_metric(result: SpeedtestResult) -> Result<SpeedtestResult> {
    if result.download_mbps.is_none() && result.upload_mbps.is_none() && result.ping_ms.is_none() {
        bail!(
            "{} output must contain download, upload, or ping metrics",
            result.provider
        );
    }
    Ok(result)
}

fn number(value: &Value, pointer: &str) -> Option<f64> {
    value.pointer(pointer).and_then(Value::as_f64)
}

fn integer(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn string(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apple_output() {
        let result = parse_apple(
            r#"{"base_rtt":12.5,"dl_throughput":100000000,"ul_throughput":25000000,"dl_bytes_transferred":10,"ul_bytes_transferred":20,"interface_name":"en0","test_endpoint":"edge.example"}"#,
        )
        .unwrap();
        assert_eq!(result.download_mbps, Some(100.0));
        assert_eq!(result.upload_mbps, Some(25.0));
        assert_eq!(result.ping_ms, Some(12.5));
        assert_eq!(result.server.unwrap().host.as_deref(), Some("edge.example"));
    }

    #[test]
    fn slow_providers_have_longer_default_timeouts() {
        assert_eq!(SpeedtestProvider::Apple.default_timeout().as_secs(), 30);
        assert_eq!(SpeedtestProvider::Ookla.default_timeout().as_secs(), 300);
        assert_eq!(SpeedtestProvider::Netflix.default_timeout().as_secs(), 300);
    }

    #[test]
    fn parses_ookla_bytes_per_second_as_mbps() {
        let result = parse_ookla(
            r#"{"ping":{"jitter":1.2,"latency":8.5},"download":{"bandwidth":12500000,"bytes":99},"upload":{"bandwidth":6250000,"bytes":55},"packetLoss":0,"server":{"id":42,"name":"Test","location":"Here","host":"test.example"},"result":{"url":"https://example/result"}}"#,
        )
        .unwrap();
        assert_eq!(result.download_mbps, Some(100.0));
        assert_eq!(result.upload_mbps, Some(50.0));
        assert_eq!(result.jitter_ms, Some(1.2));
        assert_eq!(result.server.unwrap().id, Some(42));
    }

    #[test]
    fn parses_netflix_output() {
        let result = parse_netflix(
            r#"{"downloadSpeed":52,"uploadSpeed":64,"latency":9,"bufferBloat":46,"userLocation":"Somewhere, NO"}"#,
        )
        .unwrap();
        assert_eq!(result.download_mbps, Some(52.0));
        assert_eq!(result.loaded_latency_ms, Some(46.0));
    }

    #[test]
    fn custom_output_requires_a_primary_metric() {
        assert!(parse_custom(r#"{"jitter_ms":2}"#).is_err());
        assert!(parse_apple("{}").is_err());
        assert!(parse_ookla("{}").is_err());
        assert!(parse_netflix("{}").is_err());
    }

    #[test]
    fn parses_live_network_samples() {
        let route = "gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(parse_default_interface(route).as_deref(), Some("en0"));

        let netstat = "Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes Coll\nen0 1500 <Link#11> aa:bb 10 0 123456 20 0 654321 0\n";
        assert_eq!(
            parse_interface_counters(netstat, "en0"),
            Some((123456, 654321))
        );

        let ping = "64 bytes from 1.1.1.1: icmp_seq=0 ttl=58 time=30.255 ms\n";
        assert_eq!(parse_ping(ping), Some(30.255));
    }

    #[tokio::test]
    async fn reports_custom_provider_lifecycle() {
        let options = SpeedtestOptions {
            provider: SpeedtestProvider::Custom,
            custom_command: Some(PathBuf::from("/usr/bin/printf")),
            custom_args: vec![r#"{"download_mbps":10}"#.into()],
            ..SpeedtestOptions::default()
        };
        let mut events = Vec::new();
        let result = run_with_progress(&options, |event| events.push(event))
            .await
            .unwrap();

        assert_eq!(result.download_mbps, Some(10.0));
        assert!(matches!(
            events.first(),
            Some(SpeedtestEvent::Started { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(SpeedtestEvent::Complete { .. })
        ));
    }

    #[tokio::test]
    async fn reports_failure_event() {
        let options = SpeedtestOptions {
            provider: SpeedtestProvider::Custom,
            ..SpeedtestOptions::default()
        };
        let mut events = Vec::new();
        assert!(
            run_with_progress(&options, |event| events.push(event))
                .await
                .is_err()
        );
        assert!(matches!(
            events.first(),
            Some(SpeedtestEvent::Started { .. })
        ));
        assert!(matches!(events.last(), Some(SpeedtestEvent::Failed { .. })));
    }
}
