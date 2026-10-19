use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use axum::http::header::HOST;

/// Who is really calling. Forwarding headers are only trusted from the local reverse proxy.
pub(crate) struct Client {
    pub ip: IpAddr,
    pub https: bool,
    pub host: Option<String>,
}

impl Client {
    pub(crate) fn of(peer: SocketAddr, headers: &HeaderMap) -> Self {
        let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
        let host = header(HOST.as_str()).map(str::to_owned);
        if !peer.ip().is_loopback() {
            return Self {
                ip: peer.ip(),
                https: false,
                host,
            };
        }
        let forwarded_ip = header("x-forwarded-for")
            .and_then(|list| list.rsplit(',').next())
            .and_then(|last| last.trim().parse().ok());
        Self {
            ip: forwarded_ip.unwrap_or(peer.ip()),
            https: header("x-forwarded-proto") == Some("https"),
            host: header("x-forwarded-host").map(str::to_owned).or(host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Client;
    use axum::http::{HeaderMap, HeaderValue};
    use std::net::SocketAddr;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn forwarded_headers_are_ignored_from_the_lan() {
        let peer: SocketAddr = "192.168.1.50:5000".parse().unwrap();
        let client = Client::of(
            peer,
            &headers(&[
                ("x-forwarded-for", "1.2.3.4"),
                ("x-forwarded-proto", "https"),
            ]),
        );
        assert_eq!(client.ip.to_string(), "192.168.1.50");
        assert!(!client.https);
    }

    #[test]
    fn forwarded_headers_are_used_from_the_local_proxy() {
        let peer: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let client = Client::of(
            peer,
            &headers(&[
                ("x-forwarded-for", "9.9.9.9, 1.2.3.4"),
                ("x-forwarded-proto", "https"),
            ]),
        );
        assert_eq!(client.ip.to_string(), "1.2.3.4");
        assert!(client.https);
    }
}
