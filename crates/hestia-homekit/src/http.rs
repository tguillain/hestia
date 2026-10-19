//! The tiny subset of HTTP/1.1 that HAP uses, parsed by hand because after pair-verify it
//! travels inside encrypted frames.

const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    Incomplete,
    Invalid,
    Complete(Request, usize),
}

pub(crate) fn parse(buffer: &[u8]) -> Parsed {
    let Some(head_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return if buffer.len() > MAX_HEAD {
            Parsed::Invalid
        } else {
            Parsed::Incomplete
        };
    };
    let Ok(head) = std::str::from_utf8(&buffer[..head_end]) else {
        return Parsed::Invalid;
    };
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target)) = (request_line.next(), request_line.next()) else {
        return Parsed::Invalid;
    };
    let mut content_length = 0;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            match value.trim().parse() {
                Ok(length) if length <= MAX_BODY => content_length = length,
                _ => return Parsed::Invalid,
            }
        }
    }
    let body_start = head_end + 4;
    let Some(body) = buffer.get(body_start..body_start + content_length) else {
        return Parsed::Incomplete;
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Parsed::Complete(
        Request {
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.to_owned(),
            body: body.to_vec(),
        },
        body_start + content_length,
    )
}

pub(crate) const JSON: &str = "application/hap+json";
pub(crate) const TLV: &str = "application/pairing+tlv8";

pub(crate) fn response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        207 => "Multi-Status",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        422 => "Unprocessable Entity",
        470 => "Connection Authorization Required",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut out = if status == 204 {
        format!("HTTP/1.1 204 {reason}\r\n\r\n").into_bytes()
    } else {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes()
    };
    out.extend_from_slice(body);
    out
}

pub(crate) fn event(body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "EVENT/1.0 200 OK\r\nContent-Type: {JSON}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::{Parsed, parse};

    #[test]
    fn parses_requests_with_and_without_body() {
        let raw = b"GET /characteristics?id=2.9,2.10 HTTP/1.1\r\nHost: x\r\n\r\nPUT /characteristics HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        let Parsed::Complete(first, used) = parse(raw) else {
            panic!("first request");
        };
        assert_eq!(first.path, "/characteristics");
        assert_eq!(first.query, "id=2.9,2.10");
        let Parsed::Complete(second, rest) = parse(&raw[used..]) else {
            panic!("second request");
        };
        assert_eq!(second.method, "PUT");
        assert_eq!(second.body, b"{}");
        assert_eq!(used + rest, raw.len());
    }

    #[test]
    fn waits_for_the_whole_body() {
        assert_eq!(
            parse(b"POST /pair-setup HTTP/1.1\r\nContent-Length: 5\r\n\r\nab"),
            Parsed::Incomplete
        );
        assert_eq!(
            parse(b"POST / HTTP/1.1\r\nContent-Length: 999999999\r\n\r\n"),
            Parsed::Invalid
        );
    }
}
