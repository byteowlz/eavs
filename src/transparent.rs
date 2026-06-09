//! Transparent egress proxy for the oqto sandbox network namespace.
//!
//! oqto-sandbox `NetworkMode::Proxy` puts an agent in a network namespace whose
//! TCP egress is captured and relayed here by a tiny in-namespace shim. The shim
//! recovers the agent's real destination (via `SO_ORIGINAL_DST`, which works
//! inside the namespace where the DNAT happened) and prepends a **PROXY protocol
//! v2** header before splicing the connection to this listener.
//!
//! This listener:
//! 1. parses the PROXY v2 header -> exact original destination IP:port,
//! 2. peeks the first client bytes for a hostname (TLS ClientHello SNI or HTTP
//!    `Host:` header) to drive the domain ACL,
//! 3. applies [`crate::network_acl::check_host_allowed`] under the configured
//!    posture (enforce = deny on failure / no hostname; monitor = log only),
//! 4. connects to the original destination and splices bytes (passthrough).
//!
//! Credential injection / MITM is intentionally out of scope here (tracked as
//! `eavs-sth0`); this is pure capture + allow/deny + observability.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::config::{NetworkConfig, TransparentConfig};
use crate::network_acl::check_host_allowed;

/// Max DNS message size we relay (covers EDNS0; classic 512 + headroom).
const DNS_BUF: usize = 4096;

/// Upstream DNS response timeout.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// 12-byte PROXY protocol v2 signature.
const PROXY_V2_SIG: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

/// Max bytes peeked from the client to find a hostname (covers a TLS
/// ClientHello with SNI or an HTTP request line + Host header).
const PEEK_LIMIT: usize = 8192;

/// Spawn the transparent egress listener and DNS relay if enabled. Returns
/// immediately; both run until the process exits.
pub fn spawn(transparent: TransparentConfig, network: NetworkConfig) {
    if !transparent.enabled {
        return;
    }
    let dns = transparent.clone();
    tokio::spawn(async move {
        if let Err(e) = run(transparent, network).await {
            tracing::error!("transparent egress listener stopped: {e:#}");
        }
    });
    tokio::spawn(async move {
        if let Err(e) = run_dns(dns).await {
            tracing::error!("egress DNS relay stopped: {e:#}");
        }
    });
}

/// Forward agent DNS queries to the configured upstream resolver. A dumb UDP
/// relay -- no filtering here; egress is enforced at the TCP/SNI layer, and a
/// resolved name is harmless without an allowed connection to follow.
async fn run_dns(cfg: TransparentConfig) -> std::io::Result<()> {
    let addr = format!("{}:{}", cfg.host, cfg.dns_port);
    let sock = Arc::new(UdpSocket::bind(&addr).await?);
    tracing::info!(
        "Egress DNS relay listening on {} -> {}",
        addr,
        cfg.dns_upstream
    );
    let mut buf = vec![0u8; DNS_BUF];
    loop {
        let (n, client) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("DNS relay recv failed: {e}");
                continue;
            }
        };
        let query = buf[..n].to_vec();
        let sock = Arc::clone(&sock);
        let upstream = cfg.dns_upstream.clone();
        tokio::spawn(async move {
            if let Err(e) = relay_dns_query(&sock, &upstream, &query, client).await {
                tracing::debug!("DNS relay for {client} failed: {e:#}");
            }
        });
    }
}

/// Send one query to `upstream` from a fresh ephemeral socket and relay the
/// reply back to `client` via the shared listener socket.
async fn relay_dns_query(
    listener: &UdpSocket,
    upstream: &str,
    query: &[u8],
    client: SocketAddr,
) -> std::io::Result<()> {
    let out = UdpSocket::bind("0.0.0.0:0").await?;
    out.connect(upstream).await?;
    out.send(query).await?;
    let mut resp = vec![0u8; DNS_BUF];
    let n = match tokio::time::timeout(DNS_TIMEOUT, out.recv(&mut resp)).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "upstream DNS timeout",
            ));
        }
    };
    listener.send_to(&resp[..n], client).await?;
    Ok(())
}

async fn run(transparent: TransparentConfig, network: NetworkConfig) -> std::io::Result<()> {
    let addr = format!("{}:{}", transparent.host, transparent.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!(
        "Transparent egress proxy listening on {} (posture: {})",
        addr,
        if transparent.enforce {
            "enforce"
        } else {
            "monitor"
        }
    );
    loop {
        let (inbound, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("transparent accept failed: {e}");
                continue;
            }
        };
        let network = network.clone();
        let enforce = transparent.enforce;
        tokio::spawn(async move {
            if let Err(e) = handle(inbound, network, enforce).await {
                tracing::debug!("transparent conn from {peer} ended: {e:#}");
            }
        });
    }
}

async fn handle(
    mut inbound: TcpStream,
    network: NetworkConfig,
    enforce: bool,
) -> std::io::Result<()> {
    // 1. PROXY v2 header -> original destination.
    let dst = read_proxy_v2_dst(&mut inbound).await?;

    // 2. Peek the first client bytes for a hostname.
    let mut peek = vec![0u8; PEEK_LIMIT];
    let n = inbound.read(&mut peek).await?;
    peek.truncate(n);
    let hostname = extract_hostname(&peek);

    // 3. Apply the domain ACL. Prefer the hostname; fall back to the dst IP.
    let acl_target = hostname.clone().unwrap_or_else(|| dst.ip().to_string());
    let verdict = check_host_allowed(&network, &acl_target);

    match (&verdict, enforce) {
        (Err(reason), true) => {
            tracing::info!("egress DENY {} (dst {}): {}", acl_target, dst, reason);
            return Ok(()); // drop the connection
        }
        (Err(reason), false) => {
            tracing::info!(
                "egress ALLOW (monitor) {} (dst {}): would deny: {}",
                acl_target,
                dst,
                reason
            );
        }
        (Ok(()), _) => {
            tracing::info!("egress ALLOW {} (dst {})", acl_target, dst);
        }
    }

    // 4. Connect to the exact original destination and splice.
    let mut outbound = TcpStream::connect(dst).await?;
    if !peek.is_empty() {
        outbound.write_all(&peek).await?;
    }
    copy_bidirectional(&mut inbound, &mut outbound).await?;
    Ok(())
}

/// Read and validate a PROXY protocol v2 header from `stream`, returning the
/// declared destination address. Consumes exactly the header bytes.
async fn read_proxy_v2_dst(stream: &mut TcpStream) -> std::io::Result<SocketAddr> {
    let mut head = [0u8; 16];
    stream.read_exact(&mut head).await?;
    let (addr_len, fam) = parse_proxy_v2_head(&head)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut addrs = vec![0u8; addr_len];
    stream.read_exact(&mut addrs).await?;
    parse_proxy_v2_dst(fam, &addrs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Address family from the PROXY v2 family/protocol byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyFam {
    Inet,
    Inet6,
}

/// Validate the 16-byte PROXY v2 prefix; return `(address_block_len, family)`.
fn parse_proxy_v2_head(head: &[u8; 16]) -> Result<(usize, ProxyFam), &'static str> {
    if head[..12] != PROXY_V2_SIG {
        return Err("bad PROXY v2 signature");
    }
    // byte 12: version (high nibble) must be 2; command (low nibble) must be PROXY (1).
    if head[12] >> 4 != 0x2 {
        return Err("unsupported PROXY protocol version");
    }
    if head[12] & 0x0F != 0x1 {
        return Err("unsupported PROXY command (expected PROXY)");
    }
    // byte 13: family (high nibble), transport (low nibble) must be STREAM (1).
    if head[13] & 0x0F != 0x1 {
        return Err("unsupported PROXY transport (expected STREAM)");
    }
    let fam = match head[13] >> 4 {
        0x1 => ProxyFam::Inet,
        0x2 => ProxyFam::Inet6,
        _ => return Err("unsupported PROXY address family"),
    };
    let addr_len = u16::from_be_bytes([head[14], head[15]]) as usize;
    let min = match fam {
        ProxyFam::Inet => 12,  // src4 + dst4 + sport + dport
        ProxyFam::Inet6 => 36, // src16 + dst16 + sport + dport
    };
    if addr_len < min {
        return Err("PROXY address block too short for family");
    }
    Ok((addr_len, fam))
}

/// Extract the destination address from the PROXY v2 address block.
fn parse_proxy_v2_dst(fam: ProxyFam, addrs: &[u8]) -> Result<SocketAddr, &'static str> {
    match fam {
        ProxyFam::Inet => {
            // [src(4)][dst(4)][sport(2)][dport(2)]
            let dst = Ipv4Addr::new(addrs[4], addrs[5], addrs[6], addrs[7]);
            let dport = u16::from_be_bytes([addrs[10], addrs[11]]);
            Ok(SocketAddr::new(IpAddr::V4(dst), dport))
        }
        ProxyFam::Inet6 => {
            // [src(16)][dst(16)][sport(2)][dport(2)]
            let mut d = [0u8; 16];
            d.copy_from_slice(&addrs[16..32]);
            let dport = u16::from_be_bytes([addrs[34], addrs[35]]);
            Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(d)), dport))
        }
    }
}

/// Best-effort hostname extraction from the first client bytes: TLS SNI for a
/// ClientHello, otherwise an HTTP `Host:` header. Returns `None` if neither is
/// present (e.g. a non-TLS, non-HTTP protocol).
fn extract_hostname(buf: &[u8]) -> Option<String> {
    if buf.first() == Some(&0x16) {
        extract_sni(buf)
    } else {
        extract_http_host(buf)
    }
}

/// Parse the SNI server_name from a TLS ClientHello. Fully bounds-checked;
/// returns `None` on any malformed/short input rather than panicking.
fn extract_sni(buf: &[u8]) -> Option<String> {
    // TLS record header: type(1)=0x16, version(2), length(2).
    let rec = buf.get(5..)?; // skip the 5-byte record header
                             // Handshake: msg_type(1)=0x01 (client_hello), length(3), version(2),
                             // random(32), session_id_len(1)+id, cipher_suites_len(2)+suites,
                             // compression_len(1)+methods, extensions_len(2)+extensions.
    let mut p = 0usize;
    if *rec.get(p)? != 0x01 {
        return None; // not a ClientHello
    }
    p += 4; // msg_type(1) + length(3)
    p += 2; // client version
    p += 32; // random
    let sid_len = *rec.get(p)? as usize;
    p += 1 + sid_len;
    let cs_len = u16::from_be_bytes([*rec.get(p)?, *rec.get(p + 1)?]) as usize;
    p += 2 + cs_len;
    let comp_len = *rec.get(p)? as usize;
    p += 1 + comp_len;
    let ext_total = u16::from_be_bytes([*rec.get(p)?, *rec.get(p + 1)?]) as usize;
    p += 2;
    let ext_end = p.checked_add(ext_total)?;
    if ext_end > rec.len() {
        return None;
    }
    // Walk extensions looking for server_name (type 0x0000).
    while p + 4 <= ext_end {
        let ext_type = u16::from_be_bytes([rec[p], rec[p + 1]]);
        let ext_len = u16::from_be_bytes([rec[p + 2], rec[p + 3]]) as usize;
        p += 4;
        if p + ext_len > ext_end {
            return None;
        }
        if ext_type == 0x0000 {
            // server_name_list: list_len(2), then name_type(1)=0, name_len(2), name.
            let ext = &rec[p..p + ext_len];
            if ext.len() < 5 || ext[2] != 0x00 {
                return None;
            }
            let name_len = u16::from_be_bytes([ext[3], ext[4]]) as usize;
            let name = ext.get(5..5 + name_len)?;
            return std::str::from_utf8(name).ok().map(|s| s.to_string());
        }
        p += ext_len;
    }
    None
}

/// Parse the `Host:` header from the start of an HTTP/1.x request.
fn extract_http_host(buf: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(buf).ok()?;
    // Only treat it as HTTP if it starts with a known method to avoid
    // misreading binary protocols.
    const METHODS: [&str; 7] = [
        "GET ", "POST ", "PUT ", "HEAD ", "DELETE ", "PATCH ", "OPTIONS",
    ];
    if !METHODS.iter().any(|m| text.starts_with(m)) {
        return None;
    }
    for line in text.split("\r\n").skip(1) {
        if line.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = line
            .split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.trim())
        {
            // Strip a :port suffix for the domain ACL.
            let host = rest.rsplit_once(':').map(|(h, _)| h).unwrap_or(rest);
            return Some(host.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy_v2_tcp4(dst: Ipv4Addr, dport: u16) -> Vec<u8> {
        let mut v = PROXY_V2_SIG.to_vec();
        v.push(0x21); // version 2, command PROXY
        v.push(0x11); // AF_INET, STREAM
        v.extend_from_slice(&12u16.to_be_bytes()); // address block length
        v.extend_from_slice(&[10, 0, 0, 2]); // src
        v.extend_from_slice(&dst.octets()); // dst
        v.extend_from_slice(&54321u16.to_be_bytes()); // src port
        v.extend_from_slice(&dport.to_be_bytes()); // dst port
        v
    }

    #[test]
    fn proxy_v2_head_and_dst_roundtrip() {
        let hdr = proxy_v2_tcp4(Ipv4Addr::new(140, 82, 112, 3), 443);
        let head: [u8; 16] = hdr[..16].try_into().unwrap();
        let (len, fam) = parse_proxy_v2_head(&head).unwrap();
        assert_eq!(fam, ProxyFam::Inet);
        assert_eq!(len, 12);
        let dst = parse_proxy_v2_dst(fam, &hdr[16..16 + len]).unwrap();
        assert_eq!(dst, SocketAddr::from(([140, 82, 112, 3], 443)));
    }

    #[test]
    fn proxy_v2_rejects_bad_signature() {
        let mut head = [0u8; 16];
        head[0] = 0xFF;
        assert!(parse_proxy_v2_head(&head).is_err());
    }

    #[test]
    fn proxy_v2_rejects_wrong_version() {
        let mut hdr = proxy_v2_tcp4(Ipv4Addr::LOCALHOST, 80);
        hdr[12] = 0x11; // version 1
        let head: [u8; 16] = hdr[..16].try_into().unwrap();
        assert!(parse_proxy_v2_head(&head).is_err());
    }

    #[test]
    fn sni_extracted_from_clienthello() {
        // Minimal but well-formed ClientHello carrying SNI "example.com".
        let host = b"example.com";
        let mut ch = Vec::new();
        ch.push(0x01); // client_hello
        let body_len_pos = ch.len();
        ch.extend_from_slice(&[0, 0, 0]); // handshake length placeholder
        ch.extend_from_slice(&[0x03, 0x03]); // version
        ch.extend_from_slice(&[0u8; 32]); // random
        ch.push(0); // session id len
        ch.extend_from_slice(&2u16.to_be_bytes()); // cipher suites len
        ch.extend_from_slice(&[0x00, 0x2f]); // one cipher suite
        ch.push(1); // compression methods len
        ch.push(0); // null compression
                    // Extensions: one server_name extension.
        let mut sni_ext = Vec::new();
        sni_ext.extend_from_slice(&0u16.to_be_bytes()); // ext type 0 (server_name)
        let mut sni_body = Vec::new();
        let mut name_entry = Vec::new();
        name_entry.push(0x00); // name type host_name
        name_entry.extend_from_slice(&(host.len() as u16).to_be_bytes());
        name_entry.extend_from_slice(host);
        sni_body.extend_from_slice(&(name_entry.len() as u16).to_be_bytes()); // server_name_list len
        sni_body.extend_from_slice(&name_entry);
        sni_ext.extend_from_slice(&(sni_body.len() as u16).to_be_bytes());
        sni_ext.extend_from_slice(&sni_body);
        ch.extend_from_slice(&(sni_ext.len() as u16).to_be_bytes()); // extensions total len
        ch.extend_from_slice(&sni_ext);
        // Backfill handshake length.
        let body_len = ch.len() - body_len_pos - 3;
        ch[body_len_pos..body_len_pos + 3].copy_from_slice(&[
            (body_len >> 16) as u8,
            (body_len >> 8) as u8,
            body_len as u8,
        ]);
        // Wrap in a TLS record.
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(ch.len() as u16).to_be_bytes());
        rec.extend_from_slice(&ch);

        assert_eq!(extract_hostname(&rec).as_deref(), Some("example.com"));
    }

    #[test]
    fn sni_parser_never_panics_on_garbage() {
        for len in 0..64 {
            let buf = vec![0x16u8; len];
            let _ = extract_sni(&buf); // must not panic
        }
        assert!(extract_sni(&[0x16, 0x03, 0x01, 0xff, 0xff]).is_none());
    }

    #[test]
    fn http_host_header_extracted() {
        let req = b"GET /path HTTP/1.1\r\nHost: api.github.com\r\nAccept: */*\r\n\r\n";
        assert_eq!(extract_hostname(req).as_deref(), Some("api.github.com"));
    }

    #[test]
    fn http_host_strips_port() {
        let req = b"POST / HTTP/1.1\r\nHost: example.com:8443\r\n\r\n";
        assert_eq!(extract_hostname(req).as_deref(), Some("example.com"));
    }

    #[test]
    fn non_http_non_tls_has_no_hostname() {
        assert!(extract_hostname(b"\x00\x01\x02 random binary").is_none());
    }

    #[tokio::test]
    async fn dns_relay_forwards_query_and_returns_response() {
        // Fake upstream resolver: echoes a canned reply for whatever it receives.
        let upstream = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let canned = b"RESPONSE-BYTES".to_vec();
        let canned_for_task = canned.clone();
        tokio::spawn(async move {
            let mut b = vec![0u8; DNS_BUF];
            let (_n, from) = upstream.recv_from(&mut b).await.unwrap();
            upstream.send_to(&canned_for_task, from).await.unwrap();
        });

        // The shared "listener" socket the relay uses to answer the client.
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // The client that should receive the relayed response.
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client.local_addr().unwrap();

        relay_dns_query(
            &listener,
            &upstream_addr.to_string(),
            b"QUERY-BYTES",
            client_addr,
        )
        .await
        .expect("relay should succeed");

        let mut got = vec![0u8; DNS_BUF];
        let n = client.recv(&mut got).await.unwrap();
        assert_eq!(&got[..n], canned.as_slice());
    }
}
