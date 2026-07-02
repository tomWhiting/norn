//! SSRF guard for [`super::fetch::WebFetchTool`].
//!
//! Default-deny policy for requests that address internal
//! infrastructure: loopback, link-local (including the cloud metadata
//! endpoint `169.254.169.254` and IPv6 `fe80::/10`), RFC 1918 private
//! ranges, IPv6 unique-local (`fc00::/7`), unspecified, and broadcast
//! addresses are refused — both for literal IP hosts and for **every**
//! address a hostname resolves to. IPv4-mapped IPv6 literals
//! (`::ffff:a.b.c.d`) are classified by their embedded IPv4 address so
//! they cannot smuggle a private target past the guard.
//!
//! The guard runs again on every redirect hop (the fetch tool follows
//! redirects manually for exactly this reason). Embedders that
//! legitimately fetch from private networks opt out explicitly via
//! [`super::fetch::WebFetchTool::allow_private_hosts`].
//!
//! DNS rebinding is closed by **pinning**: for hostname destinations the
//! guard returns the exact addresses it validated
//! ([`HostValidation::Pinned`]) and the fetch tool connects only to
//! those addresses (via `reqwest`'s resolver override), so an attacker
//! re-pointing a zero-TTL record between validation and connect cannot
//! swap in a private target.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use url::Url;

use crate::error::ToolError;

/// How the fetch tool must connect after host validation.
#[derive(Clone, Debug)]
pub(super) enum HostValidation {
    /// Literal-IP host, or guard opt-out: the checked value *is* the
    /// connection target, so no resolver pinning is needed.
    Unpinned,
    /// Hostname destination: connect only to the validated addresses.
    Pinned {
        /// The hostname whose resolution is pinned.
        domain: String,
        /// Every address the guard validated, in resolution order.
        addrs: Vec<SocketAddr>,
    },
}

/// Validates that `url` does not address a denied (private/internal)
/// host. A literal IP host is classified directly; a hostname is
/// resolved and **all** of its addresses must be acceptable — the
/// validated set is returned so the caller can pin the connection to it.
///
/// `allow_private` is the explicit opt-out: when `true` the check
/// passes unconditionally (and no pinning is required, since nothing was
/// validated to preserve).
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] when the URL has no host,
/// resolution fails or yields no addresses, or any address falls in a
/// denied class.
pub(super) async fn validate_url_host(
    url: &Url,
    allow_private: bool,
) -> Result<HostValidation, ToolError> {
    if allow_private {
        return Ok(HostValidation::Unpinned);
    }
    let host = url.host().ok_or_else(|| ToolError::ExecutionFailed {
        reason: format!("web_fetch refused: URL {url} has no host"),
    })?;
    match host {
        url::Host::Ipv4(ip) => {
            refuse_if_denied(url, &ip.to_string(), IpAddr::V4(ip))?;
            Ok(HostValidation::Unpinned)
        }
        url::Host::Ipv6(ip) => {
            refuse_if_denied(url, &ip.to_string(), IpAddr::V6(ip))?;
            Ok(HostValidation::Unpinned)
        }
        url::Host::Domain(domain) => {
            // Port is irrelevant to address classification but required
            // by `lookup_host`'s `ToSocketAddrs` contract.
            let port = url.port_or_known_default().unwrap_or(80);
            let addrs = tokio::net::lookup_host((domain, port)).await.map_err(|e| {
                ToolError::ExecutionFailed {
                    reason: format!("web_fetch: DNS resolution failed for {domain}: {e}"),
                }
            })?;
            let mut validated: Vec<SocketAddr> = Vec::new();
            for addr in addrs {
                refuse_if_denied(url, domain, addr.ip())?;
                validated.push(addr);
            }
            if validated.is_empty() {
                return Err(ToolError::ExecutionFailed {
                    reason: format!("web_fetch: DNS returned no addresses for {domain}"),
                });
            }
            Ok(HostValidation::Pinned {
                domain: domain.to_owned(),
                addrs: validated,
            })
        }
    }
}

/// Builds the structured refusal for `ip` when it falls in a denied class.
fn refuse_if_denied(url: &Url, host_label: &str, ip: IpAddr) -> Result<(), ToolError> {
    match denied_class(ip) {
        None => Ok(()),
        Some(class) => Err(ToolError::ExecutionFailed {
            reason: format!(
                "web_fetch refused (SSRF guard): {url} — host {host_label} resolves to {ip}, \
                 a {class} address. Private/internal destinations are denied by default; \
                 enable allow_private_hosts on the WebFetchTool to permit them."
            ),
        }),
    }
}

/// Classifies `ip`, returning the denied-class name or `None` when the
/// address is publicly routable.
pub(super) fn denied_class(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => denied_class_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return denied_class_v4(mapped);
            }
            if v6.is_loopback() {
                Some("loopback")
            } else if v6.is_unspecified() {
                Some("unspecified")
            } else if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                // fe80::/10 — stable masks used because the std
                // `is_unicast_link_local`/`is_unique_local` helpers are
                // unstable.
                Some("link-local")
            } else if (v6.segments()[0] & 0xfe00) == 0xfc00 {
                // fc00::/7
                Some("unique-local (ULA)")
            } else {
                None
            }
        }
    }
}

/// IPv4 denied classes.
fn denied_class_v4(ip: Ipv4Addr) -> Option<&'static str> {
    if ip.is_loopback() {
        Some("loopback")
    } else if ip.is_link_local() {
        Some("link-local (cloud metadata range)")
    } else if ip.is_private() {
        Some("RFC 1918 private")
    } else if ip.is_unspecified() {
        Some("unspecified")
    } else if ip.is_broadcast() {
        Some("broadcast")
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn class_of(ip: &str) -> Option<&'static str> {
        denied_class(ip.parse().unwrap())
    }

    #[test]
    fn loopback_is_denied() {
        assert_eq!(class_of("127.0.0.1"), Some("loopback"));
        assert_eq!(class_of("127.8.9.10"), Some("loopback"));
        assert_eq!(class_of("::1"), Some("loopback"));
    }

    #[test]
    fn link_local_and_metadata_are_denied() {
        assert_eq!(
            class_of("169.254.169.254"),
            Some("link-local (cloud metadata range)")
        );
        assert_eq!(
            class_of("169.254.0.1"),
            Some("link-local (cloud metadata range)")
        );
        assert_eq!(class_of("fe80::1"), Some("link-local"));
    }

    #[test]
    fn rfc1918_private_ranges_are_denied() {
        assert_eq!(class_of("10.0.0.1"), Some("RFC 1918 private"));
        assert_eq!(class_of("172.16.0.1"), Some("RFC 1918 private"));
        assert_eq!(class_of("172.31.255.255"), Some("RFC 1918 private"));
        assert_eq!(class_of("192.168.1.1"), Some("RFC 1918 private"));
    }

    #[test]
    fn ipv6_unique_local_is_denied() {
        assert_eq!(class_of("fc00::1"), Some("unique-local (ULA)"));
        assert_eq!(class_of("fd12:3456:789a::1"), Some("unique-local (ULA)"));
    }

    #[test]
    fn unspecified_and_broadcast_are_denied() {
        assert_eq!(class_of("0.0.0.0"), Some("unspecified"));
        assert_eq!(class_of("255.255.255.255"), Some("broadcast"));
        assert_eq!(class_of("::"), Some("unspecified"));
    }

    #[test]
    fn ipv4_mapped_ipv6_is_classified_by_embedded_address() {
        assert_eq!(class_of("::ffff:127.0.0.1"), Some("loopback"));
        assert_eq!(class_of("::ffff:10.0.0.1"), Some("RFC 1918 private"));
        assert_eq!(class_of("::ffff:1.1.1.1"), None);
    }

    #[test]
    fn public_addresses_are_allowed() {
        assert_eq!(class_of("1.1.1.1"), None);
        assert_eq!(class_of("8.8.8.8"), None);
        assert_eq!(class_of("93.184.216.34"), None);
        assert_eq!(class_of("2606:4700:4700::1111"), None);
    }

    #[test]
    fn boundaries_of_172_slash_12_are_correct() {
        assert_eq!(class_of("172.15.255.255"), None);
        assert_eq!(class_of("172.32.0.1"), None);
    }

    #[tokio::test]
    async fn literal_loopback_url_is_refused() {
        let url = Url::parse("http://127.0.0.1:8080/admin").unwrap();
        let err = validate_url_host(&url, false).await.unwrap_err();
        assert!(err.to_string().contains("loopback"), "{err}");
        assert!(err.to_string().contains("SSRF"), "{err}");
    }

    #[tokio::test]
    async fn literal_metadata_url_is_refused() {
        let url = Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        let err = validate_url_host(&url, false).await.unwrap_err();
        assert!(err.to_string().contains("link-local"), "{err}");
    }

    #[tokio::test]
    async fn localhost_hostname_resolves_to_denied_address() {
        let url = Url::parse("http://localhost:9999/").unwrap();
        let err = validate_url_host(&url, false).await.unwrap_err();
        assert!(err.to_string().contains("loopback"), "{err}");
    }

    #[tokio::test]
    async fn opt_out_allows_private_hosts() {
        let url = Url::parse("http://127.0.0.1:8080/").unwrap();
        assert!(matches!(
            validate_url_host(&url, true).await.unwrap(),
            HostValidation::Unpinned
        ));
        let meta = Url::parse("http://169.254.169.254/").unwrap();
        assert!(matches!(
            validate_url_host(&meta, true).await.unwrap(),
            HostValidation::Unpinned
        ));
    }

    /// Literal-IP hosts need no resolver pinning — the checked address is
    /// the connected address by construction.
    #[tokio::test]
    async fn literal_public_ip_is_unpinned() {
        let url = Url::parse("http://1.1.1.1/").unwrap();
        assert!(matches!(
            validate_url_host(&url, false).await.unwrap(),
            HostValidation::Unpinned
        ));
    }
}
