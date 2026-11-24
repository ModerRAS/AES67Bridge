//! DNS-SD / mDNS advertisement and browsing for RAVENNA.
//!
//! RAVENNA's discovery model (Operating Principles Draft 1.0 section 3.5,
//! cross-checked against the Merging/bondagit aes67-linux-daemon Avahi
//! implementation):
//!
//! * Node RTSP service: instance = vendor node id / user defined node name,
//!   type _rtsp._tcp, subtype _ravenna._sub._rtsp._tcp.
//! * Node HTTP service: type _http._tcp, subtype _ravenna._sub._http._tcp.
//! * Session service: instance = session name, type _rtsp._tcp,
//!   subtype _ravenna_session._sub._rtsp._tcp.
//! * A controller browses the *subtype* and then issues
//!   DESCRIBE rtsp://<host>:<port>/by-name/<session name>.
//!
//! mdns-sd accepts a subtype in its ty_domain argument
//! (_ravenna_session._sub._rtsp._tcp.local.) and emits both the base-type PTR
//! and the subtype PTR, which is exactly what RFC 6763 section 7.1 prescribes
//! (verified in mdns-sd 0.22.0 service_daemon.rs: base PTR is the answer record,
//! the subtype PTR is an additional answer).

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tracing::{debug, info};

pub const BASE_RTSP_TYPE: &str = "_rtsp._tcp.local.";
pub const BASE_HTTP_TYPE: &str = "_http._tcp.local.";
/// Subtype of the node's RTSP/HTTP services.
pub const NODE_RTSP_SUBTYPE: &str = "_ravenna._sub._rtsp._tcp.local.";
pub const NODE_HTTP_SUBTYPE: &str = "_ravenna._sub._http._tcp.local.";
/// Subtype of an advertised streaming session.
pub const SESSION_SUBTYPE: &str = "_ravenna_session._sub._rtsp._tcp.local.";

/// One DNS-SD registration.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub instance: String,
    /// Service type, optionally carrying a subtype label
    /// (_ravenna_session._sub._rtsp._tcp.local.).
    pub ty_domain: String,
    pub port: u16,
    pub txt: Vec<(String, String)>,
}

impl ServiceSpec {
    pub fn base_type(&self) -> &str {
        match self.ty_domain.rsplit_once("._sub.") {
            Some((_, base)) => base,
            None => &self.ty_domain,
        }
    }

    pub fn subtype(&self) -> Option<&str> {
        self.ty_domain.contains("._sub.").then_some(self.ty_domain.as_str())
    }

    pub fn fullname(&self) -> String {
        format!("{}.{}", self.instance, self.base_type())
    }
}

/// Node services: RTSP (MUST for a source) and HTTP (RECOMMENDED control page),
/// both with the _ravenna subtype (section 3.5.1).
pub fn node_specs(
    node_id: &str,
    rtsp_port: u16,
    http_port: u16,
    txt: &[(String, String)],
) -> Vec<ServiceSpec> {
    let mut rtsp_txt = txt.to_vec();
    rtsp_txt.push(("node-id".to_string(), node_id.to_string()));
    vec![
        ServiceSpec {
            instance: node_id.to_string(),
            ty_domain: NODE_RTSP_SUBTYPE.to_string(),
            port: rtsp_port,
            txt: rtsp_txt,
        },
        ServiceSpec {
            instance: node_id.to_string(),
            ty_domain: NODE_HTTP_SUBTYPE.to_string(),
            port: http_port,
            txt: vec![("node-id".to_string(), node_id.to_string())],
        },
    ]
}

/// Session service: instance = session name, MUST carry the ravenna_session
/// sub service (section 3.5.2.1).
pub fn session_specs(
    session_name: &str,
    rtsp_port: u16,
    txt: &[(String, String)],
) -> Vec<ServiceSpec> {
    vec![ServiceSpec {
        instance: session_name.to_string(),
        ty_domain: SESSION_SUBTYPE.to_string(),
        port: rtsp_port,
        txt: txt.to_vec(),
    }]
}

/// Publishes services until dropped / Announcer::shutdown.
pub struct Announcer {
    daemon: ServiceDaemon,
    registered: Vec<String>,
}

impl Announcer {
    pub fn start(specs: &[ServiceSpec], host_name: &str, ip: IpAddr) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("create mDNS daemon")?;
        let mut registered = Vec::new();
        for spec in specs {
            let txt: HashMap<String, String> = spec.txt.iter().cloned().collect();
            let info =
                ServiceInfo::new(&spec.ty_domain, &spec.instance, host_name, ip, spec.port, txt)
                    .with_context(|| {
                        format!("build ServiceInfo for {} / {}", spec.instance, spec.ty_domain)
                    })?;
            let fullname = info.get_fullname().to_string();
            daemon
                .register(info)
                .with_context(|| format!("register mDNS service {fullname}"))?;
            info!(
                fullname = %fullname,
                subtype = ?spec.subtype(),
                port = spec.port,
                "mDNS: registered"
            );
            registered.push(fullname);
        }
        Ok(Self { daemon, registered })
    }

    /// Full DNS names of everything registered.
    pub fn registered(&self) -> &[String] {
        &self.registered
    }

    pub fn shutdown(&self) {
        if let Err(e) = self.daemon.shutdown() {
            debug!("mDNS shutdown: {e}");
        }
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A service seen by the browser, as a RAVENNA controller would see it.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub instance: String,
    pub fullname: String,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
    pub txt: Vec<(String, String)>,
    pub service_type: String,
    pub subtype: Option<String>,
}

impl Discovered {
    fn from_resolved(r: &ResolvedService) -> Self {
        Self {
            instance: r.fullname.split('.').next().unwrap_or_default().to_string(),
            fullname: r.fullname.clone(),
            host: r.host.clone(),
            port: r.port,
            addresses: r.addresses.iter().map(|ip| ip.to_ip_addr()).collect(),
            txt: r
                .txt_properties
                .iter()
                .map(|p| (p.key().to_string(), p.val_str().to_string()))
                .collect(),
            service_type: r.ty_domain.clone(),
            subtype: r.sub_ty_domain.clone(),
        }
    }

    /// The RTSP URL a controller should open for a session instance.
    pub fn rtsp_url(&self) -> String {
        format!("rtsp://{}:{}/by-name/{}", self.host, self.port, self.instance)
    }

    pub fn address(&self) -> Option<IpAddr> {
        self.addresses.first().copied()
    }
}

/// Browse service_type for timeout and return the distinct services found.
///
/// Use SESSION_SUBTYPE to see sessions, NODE_RTSP_SUBTYPE for nodes.
pub fn browse_blocking(service_type: &str, timeout: Duration) -> Result<Vec<Discovered>> {
    let daemon = ServiceDaemon::new().context("create mDNS daemon")?;
    let receiver = daemon
        .browse(service_type)
        .with_context(|| format!("browse {service_type}"))?;
    let deadline = Instant::now() + timeout;
    let mut found: Vec<Discovered> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match receiver.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(resolved)) => {
                let d = Discovered::from_resolved(&resolved);
                if !found.iter().any(|f| f.fullname == d.fullname) {
                    debug!("mDNS: resolved {}", d.fullname);
                    found.push(d);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    if let Err(e) = daemon.shutdown() {
        debug!("mDNS shutdown: {e}");
    }
    Ok(found)
}
