//! The little HTTP that Wi-Fi Share speaks: one request a connection, a body with a stated length.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

const HEAD_LIMIT: usize = 16 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    /// Bytes of body that follow the head.
    pub length: u64,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let found = self
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name));
        found.map(|(_, value)| value.as_str())
    }
    pub fn param(&self, name: &str) -> &str {
        let found = self.query.iter().find(|(n, _)| n == name);
        found.map_or("", |(_, value)| value.as_str())
    }
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value)
        })
    }
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// `%41` and `+` as a browser writes them in a query.
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |i: usize| bytes.get(i).and_then(|b| (*b as char).to_digit(16));
        match (bytes[at], hex(at + 1), hex(at + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                at += 3;
            }
            (b'+', ..) => {
                out.push(b' ');
                at += 1;
            }
            (byte, ..) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(text: &str) -> Vec<(String, String)> {
    let pairs = text.split('&').filter(|pair| !pair.is_empty());
    pairs
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

/// The head of one request. A body sent in chunks is refused: browsers send files with a length.
pub fn read(stream: &mut BufReader<TcpStream>) -> io::Result<Request> {
    let mut line = String::new();
    let mut total = stream.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(bad("request line"));
    };
    let (path, rest) = target.split_once('?').unwrap_or((target, ""));
    let mut request = Request {
        method: method.to_owned(),
        path: decode(path),
        query: query(rest),
        headers: vec![],
        length: 0,
    };
    loop {
        let mut header = String::new();
        total += stream.read_line(&mut header)?;
        if total > HEAD_LIMIT {
            return Err(bad("head too long"));
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            request
                .headers
                .push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    if request.header("transfer-encoding").is_some() {
        return Err(bad("chunked body"));
    }
    let length = request.header("content-length").map(str::parse);
    request.length = length.transpose().map_err(|_| bad("length"))?.unwrap_or(0);
    Ok(request)
}

/// Read and drop a body nobody wants, so the answer reaches a browser that is still sending.
pub fn drain(stream: &mut BufReader<TcpStream>, length: u64) {
    let _ = io::copy(&mut stream.take(length.min(64 << 20)), &mut io::sink());
}

pub fn head(status: &str, kind: &str, length: u64, extra: &[String]) -> String {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {length}\r\n\
         Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n"
    );
    for line in extra {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head + "\r\n"
}

pub fn answer(out: &mut TcpStream, status: &str, kind: &str, body: &[u8], extra: &[String]) {
    let head = head(status, kind, body.len() as u64, extra);
    let _ = out
        .write_all(head.as_bytes())
        .and_then(|()| out.write_all(body));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_decode_as_a_browser_writes_them() {
        let q = query("root=sd&path=%2FMy%20Music%2FBj%C3%B6rk+1&flag");
        assert_eq!(q[0], ("root".into(), "sd".into()));
        assert_eq!(q[1], ("path".into(), "/My Music/Björk 1".into()));
        assert_eq!(q[2], ("flag".into(), String::new()));
        assert_eq!(decode("100%"), "100%");
    }
}
