//! 可注入的 HTTP 传输层（`SPEC-inference.md` §3.1：同步阻塞式）。
//!
//! 为什么自己做而不是引入 HTTP crate：
//! - 本地 sidecar 只在 `http://127.0.0.1:port` 上通信，**不需要 TLS**；
//! - 本 crate 因此保持零新增第三方依赖（许可面最小，`deny.toml` fail-closed 更省心）；
//! - 测试通过 [`Transport`] 注入桩，**不发起任何真实网络请求**（规范 §7 门禁）。
//!
//! 云端（`https://`）需要 TLS 传输：内置实现明确返回
//! [`InferenceError::TransportUnsupported`]，不在本 crate 静默降级；
//! 接入方案见 `docs/dev/research/inference.md`（v1 默认关闭云端，故不阻塞）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::InferenceError;

/// HTTP 方法（目前只用到 GET/POST）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// GET（探活 `/health`、`/v1/models`）。
    Get,
    /// POST（`/v1/chat/completions`）。
    Post,
}

impl HttpMethod {
    /// 方法文本。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// 一次 HTTP 请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// 方法。
    pub method: HttpMethod,
    /// 绝对 URL（`http://host:port/path`）。
    pub url: String,
    /// 额外请求头（不含 `Host`/`Content-Length`/`Connection`，由传输层补齐）。
    pub headers: Vec<(String, String)>,
    /// 请求体（GET 为空）。
    pub body: Vec<u8>,
    /// 读/写/连接超时。
    pub timeout: Duration,
}

impl HttpRequest {
    /// 构造 GET 请求。
    #[must_use]
    pub fn get(url: impl Into<String>, timeout: Duration) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout,
        }
    }

    /// 构造 JSON POST 请求。
    #[must_use]
    pub fn post_json(url: impl Into<String>, body: Vec<u8>, timeout: Duration) -> Self {
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body,
            timeout,
        }
    }
}

/// HTTP 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// 状态码。
    pub status: u16,
    /// 响应头（小写名）。
    pub headers: Vec<(String, String)>,
    /// 响应体。
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// 构造响应（测试用）。
    #[must_use]
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// 是否 2xx。
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// 取响应头（大小写不敏感）。
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
    }

    /// 响应体按 UTF-8 文本返回（非法字节显式失败，不做有损替换）。
    ///
    /// # Errors
    ///
    /// 响应体不是合法 UTF-8 时返回 [`InferenceError::Transport`]。
    pub fn body_text(&self) -> Result<String, InferenceError> {
        String::from_utf8(self.body.clone()).map_err(|error| InferenceError::Transport {
            detail: format!("响应体不是合法 UTF-8：{error}"),
        })
    }
}

/// 可注入的传输层抽象。
///
/// 生产：本地 sidecar 用 [`TcpTransport`]；云端需另接 TLS 实现（显式）。
/// 测试：注入 `testing::StubTransport`，注入 429/500/超时，不产生真实网络请求。
pub trait Transport: Send + Sync + core::fmt::Debug {
    /// 执行一次请求。
    ///
    /// # Errors
    ///
    /// 连接/读写失败返回 [`InferenceError::Transient`] 或 [`InferenceError::Timeout`]；
    /// 不支持的目标（如内置传输遇到 https）返回 [`InferenceError::TransportUnsupported`]。
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, InferenceError>;
}

/// 内置传输：仅支持 `http://` 明文回环/局域网地址（无 TLS）。
#[derive(Debug, Default, Clone, Copy)]
pub struct TcpTransport;

impl TcpTransport {
    /// 构造传输。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Transport for TcpTransport {
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, InferenceError> {
        let url = request.url.trim();
        let rest = url.strip_prefix("http://").ok_or_else(|| {
            if url.starts_with("https://") {
                InferenceError::TransportUnsupported {
                    detail: "内置传输不提供 TLS；云端后端需显式接入 TLS 传输（v1 默认关闭云端）"
                        .to_owned(),
                }
            } else {
                InferenceError::Transport {
                    detail: format!("只支持 http:// URL，实际为 {url}"),
                }
            }
        })?;

        let (authority, path) = rest
            .split_once('/')
            .map_or((rest, "/"), |(authority, path)| (authority, path));
        let (host, port) = split_authority(authority)?;

        let address = format!("{host}:{port}");
        let socket = address
            .to_socket_addrs()
            .map_err(|error| InferenceError::Transient {
                status: 0,
                detail: format!("解析地址 {address} 失败：{error}"),
            })?
            .next()
            .ok_or_else(|| InferenceError::Transient {
                status: 0,
                detail: format!("地址 {address} 无可用解析结果"),
            })?;

        let mut stream = TcpStream::connect_timeout(&socket, request.timeout)
            .map_err(|error| map_io_error(error, "连接本地 sidecar"))?;
        stream
            .set_read_timeout(Some(request.timeout))
            .map_err(|error| map_io_error(error, "设置读超时"))?;
        stream
            .set_write_timeout(Some(request.timeout))
            .map_err(|error| map_io_error(error, "设置写超时"))?;

        let mut head = String::new();
        head.push_str(request.method.as_str());
        head.push(' ');
        head.push('/');
        head.push_str(path);
        head.push_str(" HTTP/1.1\r\nHost: ");
        head.push_str(authority);
        head.push_str("\r\nConnection: close\r\n");
        for (name, value) in &request.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        if !request.body.is_empty() {
            head.push_str("Content-Length: ");
            head.push_str(&request.body.len().to_string());
            head.push_str("\r\n");
        }
        head.push_str("\r\n");

        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(&request.body))
            .and_then(|()| stream.flush())
            .map_err(|error| map_io_error(error, "发送请求"))?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|error| map_io_error(error, "读取状态行"))?;
        let status = parse_status_line(&status_line)?;

        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let mut line = String::new();
            let read = reader
                .read_line(&mut line)
                .map_err(|error| map_io_error(error, "读取响应头"))?;
            if read == 0 {
                break;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| InferenceError::Transport {
                    detail: format!("响应头格式非法：{line:?}"),
                })?;
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }

        let chunked = headers.iter().any(|(name, value)| {
            name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked")
        });
        let content_length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.parse::<usize>().ok());

        let body = if chunked {
            read_chunked(&mut reader, request.timeout)?
        } else if let Some(length) = content_length {
            let mut buffer = vec![0_u8; length];
            reader
                .read_exact(&mut buffer)
                .map_err(|error| map_io_error(error, "读取响应体"))?;
            buffer
        } else {
            let mut buffer = Vec::new();
            reader
                .read_to_end(&mut buffer)
                .map_err(|error| map_io_error(error, "读取响应体"))?;
            buffer
        };

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

fn split_authority(authority: &str) -> Result<(&str, u16), InferenceError> {
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port.parse::<u16>().map_err(|_| InferenceError::Transport {
                detail: format!("URL 端口非法：{authority}"),
            })?;
            Ok((host, port))
        }
        _ => Ok((authority, 80)),
    }
}

fn parse_status_line(line: &str) -> Result<u16, InferenceError> {
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(InferenceError::Transport {
            detail: format!("状态行非法：{line:?}"),
        });
    }
    code.parse::<u16>().map_err(|_| InferenceError::Transport {
        detail: format!("状态码非法：{line:?}"),
    })
}

fn read_chunked(
    reader: &mut BufReader<TcpStream>,
    timeout: Duration,
) -> Result<Vec<u8>, InferenceError> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|error| map_io_error(error, "读取分块长度"))?;
        let size_text = size_line.trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| InferenceError::Transport {
            detail: format!("分块长度非法：{size_text:?}"),
        })?;
        if size == 0 {
            // 读取尾部空行，忽略 trailer。
            let mut trailer = String::new();
            let _ = reader.read_line(&mut trailer);
            break;
        }
        let mut chunk = vec![0_u8; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| map_io_error(error, "读取分块内容"))?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0_u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|error| map_io_error(error, "读取分块结尾"))?;
        if reader.get_ref().read_timeout().is_err() {
            // 防御：极少数环境下 read_timeout 不可查询；不影响语义。
            let _ = timeout;
        }
    }
    Ok(body)
}

fn map_io_error(error: std::io::Error, stage: &str) -> InferenceError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => InferenceError::Timeout {
            stage: stage.to_owned(),
            timeout_ms: 0,
        },
        _ => InferenceError::Transient {
            status: 0,
            detail: format!("{stage}：{error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn https_is_explicitly_unsupported() {
        let transport = TcpTransport::new();
        let request =
            HttpRequest::get("https://api.deepseek.com/v1/models", Duration::from_secs(1));
        let error = transport.execute(&request).unwrap_err();
        assert!(matches!(error, InferenceError::TransportUnsupported { .. }));
        assert!(error.to_string().contains("TLS"));
    }

    #[test]
    fn authority_parsing_handles_default_and_explicit_port() {
        assert_eq!(
            split_authority("127.0.0.1:8081").unwrap(),
            ("127.0.0.1", 8081)
        );
        assert_eq!(split_authority("example.com").unwrap(), ("example.com", 80));
        assert!(split_authority("127.0.0.1:not-a-port").is_err());
    }

    #[test]
    fn status_line_parsing_rejects_garbage() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK\r\n").unwrap(), 200);
        assert!(parse_status_line("garbage").is_err());
        assert!(parse_status_line("HTTP/1.1 abc OK").is_err());
    }

    #[test]
    fn response_helpers_expose_headers_and_text() {
        let mut response = HttpResponse::new(200, b"{}".to_vec());
        response
            .headers
            .push(("content-type".to_owned(), "application/json".to_owned()));
        assert!(response.is_success());
        assert_eq!(response.header("Content-Type"), Some("application/json"));
        assert_eq!(response.body_text().unwrap(), "{}");
        assert!(!HttpResponse::new(500, b"").is_success());
    }
}
