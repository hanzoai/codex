//! Native ZAP-HTTP (`ZAP\0` v1/v2) binary zero-copy transport for `codex-http-client`.
//!
//! Speaks `github.com/zap-proto/http` v0.3.1+ directly over:
//! - Unix domain sockets: `zap+unix:///path/to/host.zap.sock:/v1/...` or `zap+unix:///path/to/host.zap.sock/v1/...`
//! - Loopback TCP: `zap://127.0.0.1:9999/v1/...` or `zap://localhost:9999/v1/...`
//!
//! Supports both single-frame responses (`FRAME_RESPONSE = 0x02`) and multi-frame
//! streaming responses (`FRAME_RESPONSE_HEAD = 0x03`, `FRAME_RESPONSE_DATA = 0x04`,
//! `FRAME_RESPONSE_END = 0x05`) so SSE (`text/event-stream`) and JSON endpoints both
//! run natively over ZAP with zero HTTP/1.1 text framing overhead.

use crate::ByteStream;
use crate::Request;
use crate::Response;
use crate::RetryAfter;
use crate::StreamResponse;
use crate::TransportError;
use bytes::Bytes;
use bytes::BytesMut;
use futures::stream;
use http::HeaderMap;
use http::StatusCode;
use http::header::HeaderName;
use http::header::HeaderValue;
use std::path::PathBuf;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use url::Host;
use url::Url;

const MAGIC: &[u8; 4] = b"ZAP\0";
const HEADER: usize = 16;
const VERSION: u16 = 1;
const VERSION_HARDENED: u16 = 2;

const FRAME_REQUEST: u16 = 0x01;
const FRAME_RESPONSE: u16 = 0x02;
const FRAME_RESPONSE_HEAD: u16 = 0x03;
const FRAME_RESPONSE_DATA: u16 = 0x04;
const FRAME_RESPONSE_END: u16 = 0x05;

const SLOTS: usize = 48;
const REQ_METHOD: usize = 0;
const REQ_TARGET: usize = 8;
const REQ_PROTO: usize = 16;
const REQ_HEADERS: usize = 24;
const REQ_BODY: usize = 32;
const RESP_STATUS: usize = 0;
const RESP_HEADERS: usize = 24;
const RESP_BODY: usize = 32;

const MAX_FRAME: u32 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZapEndpoint {
    #[cfg(unix)]
    Unix {
        socket: PathBuf,
        target: String,
    },
    Tcp {
        addr: String,
        target: String,
    },
}

pub fn is_zap_url(url: &str) -> bool {
    url.starts_with("zap://") || url.starts_with("zap+unix://") || url.starts_with("zap+tcp://")
}

pub fn validate_zap_endpoint_url(value: &str, field: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| format!("invalid {field} ZAP URL"))?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(format!(
            "{field} ZAP URL must not contain userinfo or a fragment"
        ));
    }
    match url.scheme() {
        "zap+unix" => {
            #[cfg(not(unix))]
            {
                return Err(format!("{field} zap+unix:// is only supported on Unix"));
            }
            #[cfg(unix)]
            {
                parse_zap_unix_url(&url).ok_or_else(|| {
                    format!("{field} zap+unix:// URL must specify a socket path")
                })?;
                Ok(url)
            }
        }
        "zap" | "zap+tcp" => {
            let loopback = match url.host() {
                Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                Some(Host::Ipv4(host)) => host.is_loopback(),
                Some(Host::Ipv6(host)) => host.is_loopback(),
                None => false,
            };
            if !loopback {
                return Err(format!(
                    "{field} plaintext zap:// TCP URL must target localhost/loopback"
                ));
            }
            Ok(url)
        }
        _ => Err(format!("unsupported ZAP scheme in {field}")),
    }
}

#[cfg(unix)]
fn parse_zap_unix_url(url: &Url) -> Option<(PathBuf, String)> {
    // Supports:
    // 1. `zap+unix:///path/to/host.zap.sock:/v1/models?x=1`
    // 2. `zap+unix:///path/to/host.zap.sock/v1/models?x=1` (split after `.sock`)
    let raw_path = url.path();
    if raw_path.is_empty() {
        return None;
    }
    let (sock_str, rest_path) = if let Some((sock, rest)) = raw_path.split_once(":/") {
        (sock, format!("/{rest}"))
    } else if let Some(idx) = raw_path.find(".sock/") {
        let split_at = idx + ".sock".len();
        (&raw_path[..split_at], raw_path[split_at..].to_string())
    } else if raw_path.ends_with(".sock") {
        (raw_path, "/".to_string())
    } else {
        return None;
    };
    if sock_str.is_empty() {
        return None;
    }
    let mut target = if rest_path.is_empty() {
        "/".to_string()
    } else {
        rest_path
    };
    if let Some(q) = url.query()
        && !q.is_empty()
    {
        target.push('?');
        target.push_str(q);
    }
    Some((PathBuf::from(sock_str), target))
}

pub fn parse_endpoint(raw: &str) -> Result<ZapEndpoint, TransportError> {
    let url = validate_zap_endpoint_url(raw, "request")
        .map_err(TransportError::Build)?;
    match url.scheme() {
        "zap+unix" => {
            #[cfg(unix)]
            {
                let (socket, target) = parse_zap_unix_url(&url)
                    .ok_or_else(|| TransportError::Build("invalid zap+unix socket URL".into()))?;
                Ok(ZapEndpoint::Unix { socket, target })
            }
            #[cfg(not(unix))]
            {
                Err(TransportError::Build(
                    "zap+unix is only supported on Unix".into(),
                ))
            }
        }
        "zap" | "zap+tcp" => {
            let host = url
                .host_str()
                .ok_or_else(|| TransportError::Build("missing host in ZAP URL".into()))?;
            let port = url.port().unwrap_or(9999);
            let addr = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            let mut target = if url.path().is_empty() {
                "/".to_string()
            } else {
                url.path().to_string()
            };
            if let Some(q) = url.query()
                && !q.is_empty()
            {
                target.push('?');
                target.push_str(q);
            }
            Ok(ZapEndpoint::Tcp { addr, target })
        }
        other => Err(TransportError::Build(format!(
            "unsupported ZAP scheme: {other}"
        ))),
    }
}

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

async fn connect_endpoint(endpoint: &ZapEndpoint) -> Result<Box<dyn AsyncReadWrite>, TransportError> {
    match endpoint {
        #[cfg(unix)]
        ZapEndpoint::Unix { socket, .. } => {
            let stream = tokio::net::UnixStream::connect(socket)
                .await
                .map_err(|err| TransportError::Network(format!("ZAP unix connect failed: {err}")))?;
            Ok(Box::new(stream))
        }
        ZapEndpoint::Tcp { addr, .. } => {
            let stream = TcpStream::connect(addr)
                .await
                .map_err(|err| TransportError::Network(format!("ZAP tcp connect failed: {err}")))?;
            let _ = stream.set_nodelay(true);
            Ok(Box::new(stream))
        }
    }
}

impl ZapEndpoint {
    fn target(&self) -> &str {
        match self {
            #[cfg(unix)]
            Self::Unix { target, .. } => target,
            Self::Tcp { target, .. } => target,
        }
    }
}

pub async fn execute_zap(req: Request) -> Result<Response, TransportError> {
    let url = req.url.clone();
    let max_bytes = req.response_body_limit_bytes;
    let timeout_dur = req.timeout;
    let endpoint = parse_endpoint(&url)?;
    let prepared = req.prepare_body_for_send().map_err(TransportError::Build)?;
    let body_bytes = prepared.body.unwrap_or_default();
    let frame = encode_request_frame(
        req.method.as_str(),
        endpoint.target(),
        &prepared.headers,
        &body_bytes,
    );

    let run = async {
        let mut conn = connect_endpoint(&endpoint).await?;
        write_frame(&mut conn, &frame).await?;
        let first = read_frame(&mut conn).await?;
        let decoded = decode_response_frame(&first)?;
        let status = StatusCode::from_u16(decoded.status)
            .map_err(|e| TransportError::Network(format!("invalid ZAP status {}: {e}", decoded.status)))?;
        let headers = decoded.headers;
        let mut out = BytesMut::new();
        let append = |buf: &mut BytesMut, chunk: &[u8]| -> Result<(), TransportError> {
            if let Some(limit) = max_bytes
                && buf.len().saturating_add(chunk.len()) > limit
            {
                return Err(TransportError::ResponseTooLarge { max_bytes: limit });
            }
            buf.extend_from_slice(chunk);
            Ok(())
        };
        append(&mut out, decoded.body)?;
        if decoded.frame_type == FRAME_RESPONSE_HEAD {
            loop {
                match read_frame_opt(&mut conn).await? {
                    None => break,
                    Some(next_raw) => {
                        let next = decode_response_frame(&next_raw)?;
                        append(&mut out, next.body)?;
                        if next.frame_type == FRAME_RESPONSE_END {
                            break;
                        }
                    }
                }
            }
        }
        let body = out.freeze();
        if !status.is_success() {
            let retry_after = RetryAfter::from_headers(&headers);
            return Err(TransportError::Http {
                status,
                url: Some(url),
                headers: Some(headers),
                body: String::from_utf8(body.to_vec()).ok(),
                retry_after,
            });
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    };

    match timeout_dur {
        Some(dur) => tokio::time::timeout(dur, run)
            .await
            .map_err(|_| TransportError::Timeout)?,
        None => run.await,
    }
}

pub async fn stream_zap(req: Request) -> Result<StreamResponse, TransportError> {
    let url = req.url.clone();
    let max_bytes = req.response_body_limit_bytes;
    let endpoint = parse_endpoint(&url)?;
    let prepared = req.prepare_body_for_send().map_err(TransportError::Build)?;
    let body_bytes = prepared.body.unwrap_or_default();
    let frame = encode_request_frame(
        req.method.as_str(),
        endpoint.target(),
        &prepared.headers,
        &body_bytes,
    );

    let mut conn = connect_endpoint(&endpoint).await?;
    write_frame(&mut conn, &frame).await?;
    let first = read_frame(&mut conn).await?;
    let decoded = decode_response_frame(&first)?;
    let status = StatusCode::from_u16(decoded.status)
        .map_err(|e| TransportError::Network(format!("invalid ZAP status {}: {e}", decoded.status)))?;
    let headers = decoded.headers;
    let initial_body = Bytes::copy_from_slice(decoded.body);

    if !status.is_success() {
        let retry_after = RetryAfter::from_headers(&headers);
        return Err(TransportError::Http {
            status,
            url: Some(url),
            headers: Some(headers),
            body: String::from_utf8(initial_body.to_vec()).ok(),
            retry_after,
        });
    }

    if let Some(limit) = max_bytes
        && initial_body.len() > limit
    {
        return Err(TransportError::ResponseTooLarge { max_bytes: limit });
    }

    let bytes: ByteStream = if decoded.frame_type == FRAME_RESPONSE {
        if initial_body.is_empty() {
            Box::pin(stream::empty())
        } else {
            Box::pin(stream::once(async move { Ok(initial_body) }))
        }
    } else {
        let state = (
            conn,
            (!initial_body.is_empty()).then_some(initial_body),
            false,
            0usize,
        );
        Box::pin(stream::try_unfold(
            state,
            move |(mut conn, mut pending, done, mut total)| async move {
                if done {
                    return Ok(None);
                }
                if let Some(first_chunk) = pending.take() {
                    total = total.saturating_add(first_chunk.len());
                    return Ok(Some((first_chunk, (conn, None, false, total))));
                }
                loop {
                    let Some(raw) = read_frame_opt(&mut conn).await? else {
                        return Ok(None);
                    };
                    let next = decode_response_frame(&raw)?;
                    let is_end = next.frame_type == FRAME_RESPONSE_END;
                    if !next.body.is_empty() {
                        total = total.saturating_add(next.body.len());
                        if let Some(limit) = max_bytes
                            && total > limit
                        {
                            return Err(TransportError::ResponseTooLarge { max_bytes: limit });
                        }
                        let chunk = Bytes::copy_from_slice(next.body);
                        return Ok(Some((chunk, (conn, None, is_end, total))));
                    }
                    if is_end {
                        return Ok(None);
                    }
                }
            },
        ))
    };

    Ok(StreamResponse {
        status,
        headers,
        bytes,
    })
}

pub fn encode_request_frame(
    method: &str,
    target: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Vec<u8> {
    let mut f = Vec::with_capacity(HEADER + SLOTS + target.len() + body.len() + 128);
    f.extend_from_slice(MAGIC);
    f.extend_from_slice(&VERSION.to_le_bytes());
    f.extend_from_slice(&(FRAME_REQUEST << 8).to_le_bytes());
    f.extend_from_slice(&(HEADER as u32).to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());

    let obj = f.len();
    f.resize(obj + SLOTS, 0);

    put_var(&mut f, obj + REQ_METHOD, method.as_bytes());
    put_var(&mut f, obj + REQ_TARGET, target.as_bytes());
    put_var(&mut f, obj + REQ_PROTO, b"HTTP/1.1");

    let pairs: Vec<(&[u8], &[u8])> = headers
        .iter()
        .filter(|(name, _)| {
            !name.as_str().eq_ignore_ascii_case("host")
                && !name.as_str().eq_ignore_ascii_case("content-length")
                && !name.as_str().eq_ignore_ascii_case("trailer")
        })
        .map(|(k, v)| (k.as_str().as_bytes(), v.as_bytes()))
        .collect();
    if !pairs.is_empty() {
        let start = f.len();
        f.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
        for (k, v) in pairs {
            f.extend_from_slice(&(k.len() as u32).to_le_bytes());
            f.extend_from_slice(k);
            f.extend_from_slice(&(v.len() as u32).to_le_bytes());
            f.extend_from_slice(v);
        }
        patch(&mut f, obj + REQ_HEADERS, start);
    }

    put_var(&mut f, obj + REQ_BODY, body);

    let size = f.len() as u32;
    f[12..16].copy_from_slice(&size.to_le_bytes());
    f
}

pub fn encode_response_frame(
    frame_type: u16,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut f = Vec::with_capacity(HEADER + SLOTS + body.len() + 64);
    f.extend_from_slice(MAGIC);
    f.extend_from_slice(&VERSION.to_le_bytes());
    f.extend_from_slice(&(frame_type << 8).to_le_bytes());
    f.extend_from_slice(&(HEADER as u32).to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());

    let obj = f.len();
    f.resize(obj + SLOTS, 0);
    f[obj + RESP_STATUS..obj + RESP_STATUS + 2].copy_from_slice(&status.to_le_bytes());

    if !headers.is_empty() {
        let start = f.len();
        f.extend_from_slice(&(headers.len() as u32).to_le_bytes());
        for (k, v) in headers {
            f.extend_from_slice(&(k.len() as u32).to_le_bytes());
            f.extend_from_slice(k.as_bytes());
            f.extend_from_slice(&(v.len() as u32).to_le_bytes());
            f.extend_from_slice(v.as_bytes());
        }
        patch(&mut f, obj + RESP_HEADERS, start);
    }

    put_var(&mut f, obj + RESP_BODY, body);

    let size = f.len() as u32;
    f[12..16].copy_from_slice(&size.to_le_bytes());
    f
}

struct DecodedResponse<'a> {
    frame_type: u16,
    status: u16,
    headers: HeaderMap,
    body: &'a [u8],
}

fn decode_response_frame(f: &[u8]) -> Result<DecodedResponse<'_>, TransportError> {
    let bad = |msg: String| TransportError::Network(msg);
    if f.len() < HEADER {
        return Err(bad(format!("short ZAP frame: {} bytes", f.len())));
    }
    if &f[0..4] != MAGIC {
        return Err(bad("not a ZAP frame (bad magic)".into()));
    }
    let version = u16::from_le_bytes([f[4], f[5]]);
    if version != VERSION && version != VERSION_HARDENED {
        return Err(bad(format!("unsupported ZAP wire version {version}")));
    }
    let frame_type = u16::from_le_bytes([f[6], f[7]]) >> 8;
    if !matches!(
        frame_type,
        FRAME_RESPONSE | FRAME_RESPONSE_HEAD | FRAME_RESPONSE_DATA | FRAME_RESPONSE_END
    ) {
        return Err(bad(format!("unexpected ZAP frame type {frame_type:#04x}")));
    }
    let root = u32::from_le_bytes([f[8], f[9], f[10], f[11]]) as usize;
    let size = u32::from_le_bytes([f[12], f[13], f[14], f[15]]) as usize;
    if size < HEADER || size > f.len() || root < HEADER || root + SLOTS > size {
        return Err(bad(format!("malformed ZAP frame bounds (size={size}, root={root})")));
    }
    let status_raw = u16::from_le_bytes([f[root + RESP_STATUS], f[root + RESP_STATUS + 1]]);
    let status = if status_raw == 0 && matches!(frame_type, FRAME_RESPONSE_DATA | FRAME_RESPONSE_END)
    {
        200
    } else {
        status_raw
    };
    let headers_raw = read_var(f, size, root + RESP_HEADERS)?;
    let headers = decode_headers(headers_raw)?;
    let body = read_var(f, size, root + RESP_BODY)?;
    Ok(DecodedResponse {
        frame_type,
        status,
        headers,
        body,
    })
}

fn decode_headers(buf: &[u8]) -> Result<HeaderMap, TransportError> {
    let mut map = HeaderMap::new();
    if buf.len() < 4 {
        return Ok(map);
    }
    let count = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let mut pos = 4usize;
    for _ in 0..count {
        if pos + 4 > buf.len() {
            return Err(TransportError::Network("truncated ZAP header key len".into()));
        }
        let klen = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + klen + 4 > buf.len() {
            return Err(TransportError::Network("truncated ZAP header key".into()));
        }
        let key = &buf[pos..pos + klen];
        pos += klen;
        let vlen = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + vlen > buf.len() {
            return Err(TransportError::Network("truncated ZAP header value".into()));
        }
        let val = &buf[pos..pos + vlen];
        pos += vlen;
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(key), HeaderValue::from_bytes(val)) {
            map.append(name, value);
        }
    }
    Ok(map)
}

async fn write_frame<W: AsyncWrite + Unpin + ?Sized>(
    conn: &mut W,
    frame: &[u8],
) -> Result<(), TransportError> {
    conn.write_all(&(frame.len() as u32).to_be_bytes())
        .await
        .map_err(|e| TransportError::Network(format!("ZAP write prefix failed: {e}")))?;
    conn.write_all(frame)
        .await
        .map_err(|e| TransportError::Network(format!("ZAP write frame failed: {e}")))?;
    conn.flush()
        .await
        .map_err(|e| TransportError::Network(format!("ZAP flush failed: {e}")))?;
    Ok(())
}

async fn read_frame_opt<R: AsyncRead + Unpin + ?Sized>(
    conn: &mut R,
) -> Result<Option<Vec<u8>>, TransportError> {
    let mut prefix = [0u8; 4];
    match conn.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => {
            return Err(TransportError::Network(format!(
                "ZAP read length prefix failed: {e}"
            )));
        }
    }
    let n = u32::from_be_bytes(prefix);
    if n == 0 {
        return Err(TransportError::Network("zero-length ZAP frame".into()));
    }
    if n > MAX_FRAME {
        return Err(TransportError::Network(format!(
            "ZAP frame of {n} bytes exceeds maximum {MAX_FRAME}"
        )));
    }
    let mut buf = vec![0u8; n as usize];
    conn.read_exact(&mut buf)
        .await
        .map_err(|e| TransportError::Network(format!("ZAP read frame body failed: {e}")))?;
    Ok(Some(buf))
}

async fn read_frame<R: AsyncRead + Unpin + ?Sized>(
    conn: &mut R,
) -> Result<Vec<u8>, TransportError> {
    read_frame_opt(conn)
        .await?
        .ok_or_else(|| TransportError::Network("ZAP connection closed before response".into()))
}

fn put_var(f: &mut Vec<u8>, slot: usize, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let start = f.len();
    f.extend_from_slice(data);
    patch(f, slot, start);
}

fn patch(f: &mut [u8], slot: usize, start: usize) {
    let rel = (start - slot) as u32;
    let len = (f.len() - start) as u32;
    f[slot..slot + 4].copy_from_slice(&rel.to_le_bytes());
    f[slot + 4..slot + 8].copy_from_slice(&len.to_le_bytes());
}

fn read_var<'a>(f: &'a [u8], size: usize, slot: usize) -> Result<&'a [u8], TransportError> {
    if slot + 8 > size {
        return Err(TransportError::Network("ZAP slot out of bounds".into()));
    }
    let rel = u32::from_le_bytes([f[slot], f[slot + 1], f[slot + 2], f[slot + 3]]) as usize;
    if rel == 0 {
        return Ok(&[]);
    }
    let len =
        u32::from_le_bytes([f[slot + 4], f[slot + 5], f[slot + 6], f[slot + 7]]) as usize;
    let start = slot
        .checked_add(rel)
        .ok_or_else(|| TransportError::Network("ZAP slot offset overflow".into()))?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| TransportError::Network("ZAP slot length overflow".into()))?;
    if start < HEADER || end > size {
        return Err(TransportError::Network("ZAP field runs past frame".into()));
    }
    Ok(&f[start..end])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::HttpTransport;
    use crate::ReqwestTransport;
    use futures::StreamExt;
    use http::Method;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn zap_unix_executes_unary_and_streams_sse_frames() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("host.zap.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            // 1. Unary GET /v1/models
            let (mut s1, _) = listener.accept().await.unwrap();
            let req1 = read_frame(&mut s1).await.unwrap();
            assert_eq!(&req1[0..4], MAGIC);
            let resp1 = encode_response_frame(
                FRAME_RESPONSE,
                200,
                &[("content-type", "application/json")],
                br#"{"object":"list","data":[{"id":"qwen3-coder:30b"}]}"#,
            );
            write_frame(&mut s1, &resp1).await.unwrap();

            // 2. Streaming POST /v1/responses over multi-frame ZAP SSE
            let (mut s2, _) = listener.accept().await.unwrap();
            let _req2 = read_frame(&mut s2).await.unwrap();
            let head = encode_response_frame(
                FRAME_RESPONSE_HEAD,
                200,
                &[("content-type", "text/event-stream")],
                b"",
            );
            let chunk1 = encode_response_frame(
                FRAME_RESPONSE_DATA,
                200,
                &[],
                b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            );
            let end = encode_response_frame(
                FRAME_RESPONSE_END,
                200,
                &[],
                b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            );
            write_frame(&mut s2, &head).await.unwrap();
            write_frame(&mut s2, &chunk1).await.unwrap();
            write_frame(&mut s2, &end).await.unwrap();
        });

        let transport = ReqwestTransport::new(reqwest::Client::new());
        let models_url = format!("zap+unix://{}/v1/models", sock.display());
        let res = transport
            .execute(Request::new(Method::GET, models_url))
            .await
            .unwrap();
        assert_eq!(res.status, StatusCode::OK);
        assert!(std::str::from_utf8(&res.body).unwrap().contains("qwen3-coder:30b"));

        let stream_url = format!("zap+unix://{}:/v1/responses", sock.display());
        let mut streamed = transport
            .stream(Request::new(Method::POST, stream_url))
            .await
            .unwrap();
        assert_eq!(streamed.status, StatusCode::OK);
        let mut collected = Vec::new();
        while let Some(chunk) = streamed.bytes.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        let text = String::from_utf8(collected).unwrap();
        assert!(text.contains("response.created"));
        assert!(text.contains("response.completed"));
        server.await.unwrap();
    }
}
