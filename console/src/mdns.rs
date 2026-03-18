// mDNS/Zeroconf service advertisement (dispatch-ct2.1).
//
// Advertises the console's WebSocket server as `_dispatch._tcp.local.`
// so the Android radio can discover it on the LAN without manual IP entry.

use mdns_sd::{ServiceDaemon, ServiceInfo};

const SERVICE_TYPE: &str = "_dispatch._tcp.local.";

/// Start advertising the console via mDNS. Returns the daemon handle
/// (dropping it stops advertisement).
pub fn advertise(port: u16) -> Option<ServiceDaemon> {
    let mdns = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mdns: failed to create daemon: {e}");
            return None;
        }
    };

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "dispatch-console".to_string());

    let host_label = format!("{hostname}.local.");

    let service = match ServiceInfo::new(
        SERVICE_TYPE,
        &hostname,
        &host_label,
        "",   // empty IP = auto-detect all interfaces
        port,
        None, // no TXT properties needed
    ) {
        Ok(s) => s.enable_addr_auto(),
        Err(e) => {
            eprintln!("mdns: failed to create service info: {e}");
            return None;
        }
    };

    if let Err(e) = mdns.register(service) {
        eprintln!("mdns: failed to register service: {e}");
        return None;
    }

    eprintln!("mdns: advertising {SERVICE_TYPE} on port {port}");
    Some(mdns)
}
