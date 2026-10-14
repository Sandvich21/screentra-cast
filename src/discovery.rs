use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub const DISCOVERY_PORT: u16 = 3911;
pub const DISCOVERY_PROTOCOL: &str = "screentra-cast-discovery-v1";
pub const DISCOVERY_REQUEST: &[u8] = b"SCREENTRA_CAST_DISCOVER_V1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResponse {
    pub protocol: String,
    pub server_name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_pem_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_sha256: Option<String>,
}

pub fn is_discovery_request(payload: &[u8]) -> bool {
    std::str::from_utf8(payload)
        .map(str::trim)
        .map(|value| value == std::str::from_utf8(DISCOVERY_REQUEST).unwrap())
        .unwrap_or(false)
}

pub fn build_response(
    peer: SocketAddr,
    web_port: u16,
    tls_enabled: bool,
    server_name: &str,
    ca_pem: Option<&str>,
) -> DiscoveryResponse {
    let host = match peer.ip() {
        std::net::IpAddr::V4(value) => value.to_string(),
        std::net::IpAddr::V6(value) => format!("[{value}]"),
    };
    let ca = if tls_enabled {
        ca_pem
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                let encoded = BASE64.encode(value.as_bytes());
                let digest = Sha256::digest(value.as_bytes());
                (encoded, hex_digest(&digest))
            })
    } else {
        None
    };
    DiscoveryResponse {
        protocol: DISCOVERY_PROTOCOL.to_string(),
        server_name: server_name.to_string(),
        url: format!(
            "{}://{host}:{web_port}",
            if tls_enabled { "https" } else { "http" }
        ),
        ca_pem_b64: ca.as_ref().map(|value| value.0.clone()),
        ca_sha256: ca.map(|value| value.1),
    }
}

pub fn load_published_ca(cert_path: Option<&str>) -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("SIGNAGE_TLS_CA") {
        candidates.push(std::path::PathBuf::from(path));
    }
    if let Some(cert_path) = cert_path {
        if let Some(parent) = std::path::Path::new(cert_path).parent() {
            candidates.push(parent.join("screentra-cast-signage-ca.crt"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("certs/screentra-cast-signage-ca.crt"));
            candidates.push(parent.join("static/install/screentra-cast-signage-ca.crt"));
            if let Some(root) = parent.parent() {
                candidates.push(root.join("certs/screentra-cast-signage-ca.crt"));
                candidates.push(root.join("static/install/screentra-cast-signage-ca.crt"));
            }
        }
    }
    candidates.push(std::path::PathBuf::from(
        "certs/screentra-cast-signage-ca.crt",
    ));
    candidates.push(std::path::PathBuf::from(
        "static/install/screentra-cast-signage-ca.crt",
    ));
    candidates.into_iter().find_map(|path| {
        let value = std::fs::read_to_string(path).ok()?;
        value.contains("BEGIN CERTIFICATE").then_some(value)
    })
}

pub async fn run_server(
    web_port: u16,
    tls_enabled: bool,
    server_name: String,
    ca_pem: Option<String>,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await?;
    let mut buffer = [0_u8; 256];
    log::info!(
        "LAN discovery listener active on 0.0.0.0:{}",
        DISCOVERY_PORT
    );
    loop {
        let (length, peer) = socket.recv_from(&mut buffer).await?;
        if !is_discovery_request(&buffer[..length]) {
            continue;
        }
        let response = build_response(peer, web_port, tls_enabled, &server_name, ca_pem.as_deref());
        let payload = match serde_json::to_vec(&response) {
            Ok(payload) => payload,
            Err(error) => {
                log::warn!("could not serialize LAN discovery response: {error}");
                continue;
            }
        };
        if let Err(error) = socket.send_to(&payload, peer).await {
            log::debug!("could not send LAN discovery response to {peer}: {error}");
        }
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn builds_https_discovery_response_for_ipv4_peer() {
        let response = build_response(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42)), 45678),
            3000,
            true,
            "Screentra Cast",
            Some("-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----\n"),
        );

        assert_eq!(response.protocol, DISCOVERY_PROTOCOL);
        assert_eq!(response.server_name, "Screentra Cast");
        assert_eq!(response.url, "https://10.0.0.42:3000");
        assert!(response.ca_pem_b64.is_some());
        let ca = BASE64
            .decode(response.ca_pem_b64.as_deref().unwrap())
            .unwrap();
        let digest = Sha256::digest(ca);
        assert_eq!(
            response.ca_sha256.as_deref(),
            Some(hex_digest(&digest).as_str())
        );
    }

    #[test]
    fn builds_plain_http_response_without_publishing_trust_material() {
        let response = build_response(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234),
            3000,
            false,
            "Test",
            Some("not used"),
        );

        assert_eq!(response.url, "http://127.0.0.1:3000");
        assert!(response.ca_pem_b64.is_none());
        assert!(response.ca_sha256.is_none());
    }

    #[test]
    fn rejects_untrusted_or_malformed_discovery_requests() {
        assert!(is_discovery_request(DISCOVERY_REQUEST));
        assert!(is_discovery_request(b" SCREENTRA_CAST_DISCOVER_V1\n"));
        assert!(!is_discovery_request(b"SCREENTRA_CAST_DISCOVER_V0"));
        assert!(!is_discovery_request(b"SCREENTRA_CAST_DISCOVER_V1 extra"));
    }
}
