//! Network policy governance.
//!
//! This module defines rules for controlling outbound network access from the agent.
//! By default, all network access is denied unless explicitly allowed.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// Network policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Policy version (UUID).
    pub version: String,
    /// List of allowed domains (supports wildcards like `*.google.com`).
    pub allow_domains: Vec<String>,
    /// List of explicitly denied domains (takes precedence over allow).
    pub deny_domains: Vec<String>,
    /// List of allowed destination ports.
    pub allow_ports: Vec<u16>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            version: uuid::Uuid::new_v4().to_string(),
            allow_domains: vec![],
            deny_domains: vec![],
            allow_ports: vec![80, 443], // HTTP/HTTPS allowed by default if domain matches
        }
    }
}

/// Network access decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkDecision {
    /// Access allowed.
    Allowed,
    /// Access denied with reason.
    Denied(String),
}

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

impl NetworkPolicy {
    /// Create a new network policy.
    pub fn new(
        allow_domains: Vec<String>,
        deny_domains: Vec<String>,
        allow_ports: Vec<u16>,
    ) -> Self {
        Self {
            version: uuid::Uuid::new_v4().to_string(),
            allow_domains,
            deny_domains,
            allow_ports,
        }
    }

    /// Check if a URL is allowed by the policy.
    ///
    /// Rules:
    /// 1. Port must be in `allow_ports`.
    /// 2. Host must not be a direct IP (enforce DNS usage).
    /// 3. Domain must NOT be in `deny_domains`.
    /// 4. Domain MUST be in `allow_domains`.
    pub fn check(&self, url_str: &str) -> Result<NetworkDecision, NetworkError> {
        let url = Url::parse(url_str).map_err(|e| NetworkError::InvalidUrl(e.to_string()))?;

        // 1. Check Port
        let port = url.port_or_known_default().unwrap_or(80);
        if !self.allow_ports.contains(&port) {
            return Ok(NetworkDecision::Denied(format!(
                "Port {} is not allowed",
                port
            )));
        }

        // 2. Check Host
        let host = match url.host_str() {
            Some(h) => h,
            None => return Ok(NetworkDecision::Denied("URL has no host".to_string())),
        };

        if host.parse::<std::net::IpAddr>().is_ok() {
            return Ok(NetworkDecision::Denied(
                "Direct IP access is prohibited. Use domain names.".to_string(),
            ));
        }

        // 3. Check Deny List
        for rule in &self.deny_domains {
            if self.matches(host, rule) {
                return Ok(NetworkDecision::Denied(format!(
                    "Domain '{}' is explicitly denied by rule '{}'",
                    host, rule
                )));
            }
        }

        // 4. Check Allow List
        for rule in &self.allow_domains {
            if self.matches(host, rule) {
                return Ok(NetworkDecision::Allowed);
            }
        }

        Ok(NetworkDecision::Denied(format!(
            "Domain '{}' is not in the allowlist",
            host
        )))
    }

    /// Check if an IP address is allowed (must be public).
    pub fn check_ip(&self, ip: std::net::IpAddr) -> Result<(), String> {
        match ip {
            std::net::IpAddr::V4(ipv4) => Self::check_ipv4(ipv4),
            std::net::IpAddr::V6(ipv6) => {
                // Handle IPv4-mapped IPv6 addresses (::ffff:0:0/96)
                if let Some(ipv4) = ipv6.to_ipv4() {
                    return Self::check_ipv4(ipv4);
                }

                if ipv6.is_loopback()
                    || ipv6.is_unspecified()
                    // unique local: fc00::/7
                    || (ipv6.segments()[0] & 0xfe00) == 0xfc00
                    // link local: fe80::/10
                    || (ipv6.segments()[0] & 0xffc0) == 0xfe80
                    // discarded: 100::/64
                    || (ipv6.segments()[0] == 0x0100 && ipv6.segments()[1] == 0 && ipv6.segments()[2] == 0 && ipv6.segments()[3] == 0)
                    // documentation: 2001:db8::/32
                    || (ipv6.segments()[0] == 0x2001 && ipv6.segments()[1] == 0x0db8)
                {
                    return Err(format!("Blocked internal/private IPv6: {}", ipv6));
                }
                Ok(())
            }
        }
    }

    fn check_ipv4(ipv4: std::net::Ipv4Addr) -> Result<(), String> {
        if ipv4.is_loopback()
            || ipv4.is_private()
            || ipv4.is_link_local()
            || ipv4.is_broadcast()
            || ipv4.is_documentation()
            || ipv4.is_unspecified()
        {
            return Err(format!("Blocked internal/private IPv4: {}", ipv4));
        }

        let octets = ipv4.octets();

        // Carrier-grade NAT (100.64.0.0/10)
        // 100.64.0.0 to 100.127.255.255
        if octets[0] == 100 && (octets[1] & 0xC0) == 0x40 {
            return Err(format!("Blocked Carrier-Grade NAT IPv4: {}", ipv4));
        }

        // IETF Protocol Assignments (192.0.0.0/24)
        if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
            return Err(format!("Blocked IETF Protocol Assignment IPv4: {}", ipv4));
        }

        // Benchmarking (198.18.0.0/15)
        // 198.18.0.0 to 198.19.255.255
        if octets[0] == 198 && (octets[1] & 0xFE) == 18 {
            return Err(format!("Blocked Benchmarking IPv4: {}", ipv4));
        }

        // Reserved (240.0.0.0/4) - Class E (except broadcast 255.255.255.255 which is covered)
        // 240.0.0.0 to 255.255.255.254
        if octets[0] >= 240 {
            // 255.255.255.255 is broadcast, already checked.
            // But let's block the rest of Class E just in case.
            if ipv4 != std::net::Ipv4Addr::BROADCAST {
                return Err(format!("Blocked Reserved/Class E IPv4: {}", ipv4));
            }
        }

        Ok(())
    }

    /// Helper to match domains with wildcards.
    /// Supported usage: `example.com`, `*.example.com`, `*`.
    fn matches(&self, domain: &str, rule: &str) -> bool {
        if rule == "*" {
            return true;
        }

        if let Some(suffix) = rule.strip_prefix("*.") {
            return domain.ends_with(suffix) || domain == suffix;
        }

        domain == rule
    }
}

// =============================================================================
// Egress Logic
// =============================================================================

use multi_agent_core::config::SafetyConfig;

const MAX_REDIRECTS: usize = 5;

/// Helper to perform request with manual redirect handling and SSRF protection.
/// Supports strict network policy checks at every hop.
pub async fn fetch_with_policy(
    client: &reqwest::Client,
    policy: &NetworkPolicy,
    safety: &SafetyConfig,
    mut method: reqwest::Method,
    mut url: url::Url,
    headers: Option<&reqwest::header::HeaderMap>,
    body: Option<&String>,
) -> multi_agent_core::Result<reqwest::Response> {
    for _ in 0..MAX_REDIRECTS {
        // 1. Check Policy (Domain)
        match policy.check(url.as_str()) {
            Ok(NetworkDecision::Allowed) => {}
            Ok(NetworkDecision::Denied(reason)) => {
                return Err(multi_agent_core::Error::governance(format!(
                    "Network policy denied access to {}: {}",
                    url, reason
                )));
            }
            Err(e) => {
                return Err(multi_agent_core::Error::governance(format!(
                    "Policy check failed: {}",
                    e
                )))
            }
        }

        // 2. Resolve IP & Check
        let host = url
            .host_str()
            .ok_or_else(|| multi_agent_core::Error::governance("URL has no host".to_string()))?;
        let port = url.port_or_known_default().unwrap_or(80);
        let addr_str = format!("{}:{}", host, port);

        let mut addrs = tokio::net::lookup_host(&addr_str).await.map_err(|e| {
            multi_agent_core::Error::governance(format!(
                "DNS resolution failed for {}: {}",
                host, e
            ))
        })?;

        // Use first IP
        let target_socket = addrs.next().ok_or_else(|| {
            multi_agent_core::Error::governance(format!("No IP addresses found for {}", host))
        })?;
        let target_ip = target_socket.ip();

        // Validate IP
        if let Err(e) = policy.check_ip(target_ip) {
            return Err(multi_agent_core::Error::governance(format!(
                "Network policy denied IP {}: {}",
                target_ip, e
            )));
        }

        // 3. Prepare Request (IP Pinning)
        let mut safe_url = url.clone();
        if safe_url.set_host(Some(&target_ip.to_string())).is_err() {
            return Err(multi_agent_core::Error::governance(format!(
                "Failed to set safe IP host: {}",
                target_ip
            )));
        }

        let mut req_builder = client
            .request(method.clone(), safe_url)
            .header("Host", host);

        if let Some(h) = headers {
            req_builder = req_builder.headers(h.clone());
        }

        // Only attach body if method allows AND we aren't redirected to GET
        if method != reqwest::Method::GET {
            if let Some(b) = body {
                req_builder = req_builder.body(b.clone());
            }
        }

        let resp = req_builder
            .send()
            .await
            .map_err(|e| multi_agent_core::Error::governance(format!("Request failed: {}", e)))?;

        // 4. Handle Redirects
        if resp.status().is_redirection() {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                let loc_str = loc.to_str().map_err(|e| {
                    multi_agent_core::Error::governance(format!("Invalid Location header: {}", e))
                })?;
                // Parse relative or absolute
                let next_url = url.join(loc_str).map_err(|e| {
                    multi_agent_core::Error::governance(format!(
                        "Invalid redirect URL {}: {}",
                        loc_str, e
                    ))
                })?;

                // Determine next method
                let status = resp.status();
                if status == reqwest::StatusCode::MOVED_PERMANENTLY || // 301
                   status == reqwest::StatusCode::FOUND ||             // 302
                   status == reqwest::StatusCode::SEE_OTHER
                {
                    // 303
                    method = reqwest::Method::GET;
                    // Body will be dropped in next iteration due to check above
                }

                url = next_url;
                continue;
            }
        }

        // Check Content-Length if present
        if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
            if let Ok(cl_str) = cl.to_str() {
                if let Ok(size) = cl_str.parse::<u64>() {
                    if size > safety.max_download_size_bytes {
                        return Err(multi_agent_core::Error::governance(format!(
                            "Content-Length {} exceeds limit {}",
                            size, safety.max_download_size_bytes
                        )));
                    }
                }
            }
        }

        // Check Content-Type if present
        if !safety.allowed_content_types.is_empty() {
            if let Some(ct) = resp.headers().get(reqwest::header::CONTENT_TYPE) {
                let ct_str = ct.to_str().unwrap_or("");
                let mut allowed = false;
                for a in &safety.allowed_content_types {
                    if ct_str.starts_with(a) {
                        allowed = true;
                        break;
                    }
                }
                if !allowed {
                    return Err(multi_agent_core::Error::governance(format!(
                        "Content-Type '{}' not allowed",
                        ct_str
                    )));
                }
            }
        }

        return Ok(resp);
    }

    Err(multi_agent_core::Error::governance(format!(
        "Too many redirects (max {})",
        MAX_REDIRECTS
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_deny() {
        let policy = NetworkPolicy::default();
        let result = policy.check("https://google.com").unwrap();
        assert!(matches!(result, NetworkDecision::Denied(_)));
    }

    #[test]
    fn test_allow_domain() {
        let policy = NetworkPolicy::new(vec!["google.com".to_string()], vec![], vec![443]);
        let result = policy.check("https://google.com").unwrap();
        assert_eq!(result, NetworkDecision::Allowed);
    }

    #[test]
    fn test_wildcard_allow() {
        let policy = NetworkPolicy::new(vec!["*.google.com".to_string()], vec![], vec![443]);
        assert_eq!(
            policy.check("https://mail.google.com").unwrap(),
            NetworkDecision::Allowed
        );
        assert_eq!(
            policy.check("https://google.com").unwrap(),
            NetworkDecision::Allowed
        );
        assert!(matches!(
            policy.check("https://yahoo.com").unwrap(),
            NetworkDecision::Denied(_)
        ));
    }

    #[test]
    fn test_explicit_deny_precedence() {
        let policy = NetworkPolicy::new(
            vec!["*.google.com".to_string()],
            vec!["mail.google.com".to_string()],
            vec![443],
        );
        // Explicitly denied
        let result = policy.check("https://mail.google.com").unwrap();
        assert!(
            matches!(result, NetworkDecision::Denied(reason) if reason.contains("explicitly denied"))
        );

        // Allowed by wildcard
        assert_eq!(
            policy.check("https://maps.google.com").unwrap(),
            NetworkDecision::Allowed
        );
    }

    #[test]
    fn test_port_restriction() {
        let policy = NetworkPolicy::new(vec!["google.com".to_string()], vec![], vec![443]);
        // Port 80 not allowed
        let result = policy.check("http://google.com").unwrap(); // http implies 80
        assert!(matches!(result, NetworkDecision::Denied(reason) if reason.contains("Port 80")));
    }

    #[test]
    fn test_ip_block() {
        let policy = NetworkPolicy::new(vec!["*".to_string()], vec![], vec![443]);
        let result = policy.check("https://1.1.1.1").unwrap();
        assert!(
            matches!(result, NetworkDecision::Denied(reason) if reason.contains("Direct IP access"))
        );
    }

    #[test]
    fn test_ssrf_blocks() {
        let policy = NetworkPolicy::default();

        // IPv4-mapped IPv6 Loopback
        let ip: std::net::IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(
            policy.check_ip(ip).is_err(),
            "Should block IPv4-mapped loopback"
        );

        // IPv4-mapped IPv6 Private
        let ip: std::net::IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert!(
            policy.check_ip(ip).is_err(),
            "Should block IPv4-mapped private"
        );

        // Carrier-Grade NAT
        let ip: std::net::IpAddr = "100.64.0.1".parse().unwrap();
        assert!(policy.check_ip(ip).is_err(), "Should block CGNAT");

        // Cloud Metadata
        let ip: std::net::IpAddr = "169.254.169.254".parse().unwrap();
        assert!(policy.check_ip(ip).is_err(), "Should block Metadata");

        // Benchmarking
        let ip: std::net::IpAddr = "198.18.0.1".parse().unwrap();
        assert!(policy.check_ip(ip).is_err(), "Should block Benchmarking");

        // Class E (Reserved)
        let ip: std::net::IpAddr = "240.0.0.1".parse().unwrap();
        assert!(policy.check_ip(ip).is_err(), "Should block Class E");

        // IPv6 Unique Local
        let ip: std::net::IpAddr = "fc00::1".parse().unwrap();
        assert!(
            policy.check_ip(ip).is_err(),
            "Should block IPv6 Unique Local"
        );

        // Public IP (Cloudflare DNS) - Should Pass
        let ip: std::net::IpAddr = "1.1.1.1".parse().unwrap();
        assert!(policy.check_ip(ip).is_ok(), "Should allow public IP");
    }
}
