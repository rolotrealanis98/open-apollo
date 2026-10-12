//! Bonjour discovery of `_uamixer._tcp` engines (feature `mdns`).
//!
//! Bonjour names do not tell a real engine from an open-apollo daemon: both advertise the same
//! service type.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::connection::Endpoint;
use crate::error::{Error, Result};

/// An engine found on the LAN.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    /// Instance name, e.g. `Rolando’s Mac Studio (1419)`.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<std::net::IpAddr>,
}

impl Discovered {
    /// An endpoint for the first IPv4 address (else the first address, else the host name).
    pub fn endpoint(&self) -> Endpoint {
        let host = self
            .addresses
            .iter()
            .find(|a| a.is_ipv4())
            .or(self.addresses.first())
            .map(|a| a.to_string())
            .unwrap_or_else(|| self.host.trim_end_matches('.').to_owned());
        Endpoint {
            name: self.name.clone(),
            ..Endpoint::new(host, self.port)
        }
    }
}

/// Browses for `duration` and returns what was resolved, sorted by name.
pub fn browse(duration: Duration) -> Result<Vec<Discovered>> {
    let daemon = ServiceDaemon::new().map_err(|e| Error::Other(e.to_string()))?;
    let ty = format!("{}.local.", crate::SERVICE_TYPE);
    let rx = daemon
        .browse(&ty)
        .map_err(|e| Error::Other(e.to_string()))?;
    let deadline = Instant::now() + duration;
    let mut found: BTreeMap<String, Discovered> = BTreeMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let fullname = info.fullname.clone();
                let name = fullname
                    .strip_suffix(&format!(".{ty}"))
                    .unwrap_or(&fullname)
                    .to_owned();
                let addresses = info.addresses.iter().map(|a| a.to_ip_addr()).collect();
                found.insert(
                    name.clone(),
                    Discovered {
                        name,
                        host: info.host.clone(),
                        port: info.port,
                        addresses,
                    },
                );
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}
