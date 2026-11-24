//! CLI for the RAVENNA discovery prototype.
//!
//! STATUS: paused by the user on 2026-11-24; the code is kept so the work can
//! resume without redoing the research. Nothing here has been run against the Z9X
//! or a real RAVENNA controller. See
//! artifacts/aes67/ravenna-mdns-rtsp-2026-11-24.md.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use ravenna_discovery::config::{self, NodeConfig};
use ravenna_discovery::mdns;
use ravenna_discovery::rtsp_server::{NoopPlayback, RtspServer, RtspSession, ServerConfig};
use ravenna_discovery::sdp::{self, SessionSdp, SdpProvider, StaticSdp, TsRefClk};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "ravenna-discovery",
    version,
    about = "RAVENNA DNS-SD advertisement + minimal RTSP/SDP server (prototype, PAUSED)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the SDP document that DESCRIBE would return.
    Sdp(SdpArgs),
    /// Publish the node + session services over mDNS and keep them up.
    Advertise {
        #[command(flatten)]
        sdp: SdpArgs,
        /// Exit after N seconds (default: run until Ctrl-C).
        #[arg(long)]
        duration_secs: Option<u64>,
    },
    /// Run the RTSP server (optionally announcing the services over mDNS).
    Serve {
        #[command(flatten)]
        sdp: SdpArgs,
        #[arg(long)]
        advertise: bool,
    },
    /// Browse RAVENNA services the way a controller would.
    Browse {
        #[arg(long, default_value = "_ravenna_session._sub._rtsp._tcp.local.")]
        service_type: String,
        #[arg(long, default_value_t = 5)]
        timeout_secs: u64,
    },
}

#[derive(Args, Debug, Clone)]
struct SdpArgs {
    /// Vendor node id / user defined node name (DNS-SD instance of the node RTSP service).
    #[arg(long, default_value = "AES67Bridge 0001")]
    node_id: String,
    /// Local IPv4 to advertise (default: route lookup).
    #[arg(long)]
    iface: Option<IpAddr>,
    #[arg(long, default_value_t = config::DEFAULT_RTSP_PORT)]
    rtsp_port: u16,
    #[arg(long, default_value_t = config::DEFAULT_HTTP_PORT)]
    http_port: u16,
    #[arg(long, default_value = "Line 1")]
    session_name: String,
    #[arg(long, default_value_t = 1)]
    session_id: u64,
    #[arg(long, default_value = "239.69.67.1")]
    dest: IpAddr,
    #[arg(long, default_value_t = 5004)]
    rtp_port: u16,
    #[arg(long, default_value_t = 97)]
    payload_type: u8,
    /// L16 or L24.
    #[arg(long, default_value = "L24")]
    encoding: String,
    #[arg(long, default_value_t = 48000)]
    sample_rate: u32,
    #[arg(long, default_value_t = 2)]
    channels: u16,
    /// Packet time in ms (1.0 = 1 ms, 0.125 = 125 us).
    #[arg(long, default_value_t = 1.0)]
    ptime_ms: f64,
    #[arg(long, default_value_t = 0x4E53_5000)]
    ssrc: u32,
    /// Emit RAVENNA a=clock-domain:PTPv2 N. Only valid when RTP timestamps are
    /// PTP-epoch frames, not sample counters.
    #[arg(long)]
    clock_domain: Option<u8>,
    /// PTP grandmaster id for a=ts-refclk:ptp=... (e.g. ae6f0d.fffe.8e86fc).
    #[arg(long)]
    ptp_gmid: Option<String>,
    /// Serve this SDP file verbatim instead of generating one.
    #[arg(long)]
    sdp_file: Option<PathBuf>,
    /// TXT key=value pairs added to the advertised services.
    #[arg(long = "txt", value_name = "KEY=VALUE")]
    txt: Vec<String>,
}

impl SdpArgs {
    fn local_ip(&self) -> Result<IpAddr> {
        match self.iface {
            Some(ip) => Ok(ip),
            None => config::detect_local_ip(),
        }
    }

    fn session_config(&self) -> Result<sdp::SessionConfig> {
        let encoding =
            sdp::Encoding::parse(&self.encoding).context("--encoding must be L16 or L24")?;
        let ts_refclk = match (&self.ptp_gmid, self.clock_domain) {
            (Some(gmid), Some(domain)) => TsRefClk::Ptp {
                gmid: config::parse_gmid(gmid)?,
                domain,
            },
            _ => TsRefClk::Local,
        };
        Ok(config::build_session(
            &self.session_name,
            self.session_id,
            self.local_ip()?,
            self.dest,
            self.rtp_port,
            self.payload_type,
            encoding,
            self.sample_rate,
            self.channels,
            self.ptime_ms,
            self.ssrc,
            self.clock_domain,
            ts_refclk,
        ))
    }

    fn node_config(&self) -> Result<NodeConfig> {
        Ok(NodeConfig {
            node_id: self.node_id.clone(),
            host_name: config::default_host_name(),
            local_ip: self.local_ip()?,
            rtsp_port: self.rtsp_port,
            http_port: self.http_port,
            sessions: vec![self.session_config()?],
        })
    }

    fn provider(&self) -> Result<Arc<dyn SdpProvider>> {
        if let Some(path) = &self.sdp_file {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("read SDP file {}", path.display()))?;
            return Ok(Arc::new(StaticSdp(text)));
        }
        Ok(Arc::new(SessionSdp(self.session_config()?)))
    }

    fn txt_pairs(&self) -> Vec<(String, String)> {
        self.txt
            .iter()
            .filter_map(|kv| {
                kv.split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Sdp(args) => {
            let provider = args.provider()?;
            print!("{}", provider.sdp());
        }
        Command::Advertise {
            sdp: args,
            duration_secs,
        } => {
            let node = args.node_config()?;
            let specs = services(&node, &args.txt_pairs());
            let announcer = mdns::Announcer::start(&specs, &node.host_name, node.local_ip)?;
            for name in announcer.registered() {
                println!("registered: {name}");
            }
            wait(duration_secs).await;
            announcer.shutdown();
        }
        Command::Serve {
            sdp: args,
            advertise,
        } => {
            let node = args.node_config()?;
            let session = RtspSession {
                id: args.session_id as u8,
                name: args.session_name.clone(),
                sdp: args.provider()?,
            };
            let cfg = ServerConfig {
                bind: SocketAddr::new(node.local_ip, node.rtsp_port),
                node_id: node.node_id.clone(),
                sessions: vec![session],
                playback: Arc::new(NoopPlayback),
            };
            let _announcer = if advertise {
                let specs = services(&node, &args.txt_pairs());
                Some(mdns::Announcer::start(
                    &specs,
                    &node.host_name,
                    node.local_ip,
                )?)
            } else {
                None
            };
            RtspServer::new(cfg).serve().await?;
        }
        Command::Browse {
            service_type,
            timeout_secs,
        } => {
            let found = mdns::browse_blocking(&service_type, Duration::from_secs(timeout_secs))?;
            if found.is_empty() {
                println!("no services of type {service_type}");
            }
            for d in found {
                println!("instance   : {}", d.instance);
                println!(
                    "  type     : {}{}",
                    d.service_type,
                    d.subtype
                        .clone()
                        .map(|s| format!(" (subtype {s})"))
                        .unwrap_or_default()
                );
                println!("  host/port: {}:{}", d.host, d.port);
                println!("  addresses: {:?}", d.addresses);
                println!("  rtsp     : {}", d.rtsp_url());
                for (k, v) in &d.txt {
                    println!("  txt      : {k}={v}");
                }
            }
        }
    }
    Ok(())
}

fn services(node: &NodeConfig, txt: &[(String, String)]) -> Vec<mdns::ServiceSpec> {
    let mut specs = mdns::node_specs(&node.node_id, node.rtsp_port, node.http_port, txt);
    for session in &node.sessions {
        specs.extend(mdns::session_specs(&session.session_name, node.rtsp_port, txt));
    }
    specs
}

async fn wait(duration_secs: Option<u64>) {
    match duration_secs {
        Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
        None => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
