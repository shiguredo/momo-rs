//! MOQT の接続先 URL のパース
//!
//! `moqt://host:port/path` 形式を受け取り、SETUP の AUTHORITY / PATH に載せる値と
//! QUIC 接続に使うアドレスを取り出す。

use crate::moq::error::MoqError;

/// MOQT のデフォルトポート
const DEFAULT_PORT: u16 = 4433;

/// パース済みの接続先
#[derive(Debug, Clone)]
pub struct ServerUrl {
    /// ホスト名 (IPv6 リテラルは括弧を外した形)
    host: String,
    /// ポート番号
    port: u16,
    /// `host:port` 形式の authority
    authority: String,
    /// リクエストパス。`/` の場合は `None`
    path: Option<String>,
}

impl ServerUrl {
    /// `moqt://` URL をパースする
    ///
    /// スキームは `moqt` のみを受け付ける。ポートを省略した場合は [`DEFAULT_PORT`] を使う。
    /// パスは `/` 単体の場合に `None` とし、SETUP の PATH に載せない。
    pub fn parse(url: &str) -> Result<Self, MoqError> {
        let rest = url
            .strip_prefix("moqt://")
            .ok_or_else(|| MoqError::Url(format!("URL must start with 'moqt://': {url}")))?;

        // パスとクエリを分離する。PATH は query を含めて送る必要がある
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], Some(&rest[index..])),
            None => (rest, None),
        };

        if authority.is_empty() {
            return Err(MoqError::Url(format!("authority is empty: {url}")));
        }

        // IPv6 リテラルは [::1]:4433 の形式なので括弧を考慮して分割する
        let (host, port) = if let Some(end) = authority.rfind(']') {
            let host = authority
                .strip_prefix('[')
                .and_then(|value| value.get(..end - 1))
                .ok_or_else(|| MoqError::Url(format!("invalid IPv6 literal: {authority}")))?;
            let port = match authority.get(end + 1..) {
                Some(value) if value.starts_with(':') => parse_port(&value[1..], url)?,
                Some("") => DEFAULT_PORT,
                _ => DEFAULT_PORT,
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_string(), parse_port(port, url)?),
                None => (authority.to_string(), DEFAULT_PORT),
            }
        };

        if host.is_empty() {
            return Err(MoqError::Url(format!("host is empty: {url}")));
        }

        // パスが `/` 単体の場合は PATH に載せない
        let path = path
            .filter(|value| *value != "/")
            .map(|value| value.to_string());

        Ok(Self {
            host,
            port,
            authority: authority.to_string(),
            path,
        })
    }

    /// ホスト名を返す
    pub fn host(&self) -> &str {
        &self.host
    }

    /// ポート番号を返す
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `host:port` 形式の authority を返す
    ///
    /// SETUP の AUTHORITY オプションにそのまま載せる。
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// SETUP の PATH オプションに載せる値を返す
    pub fn path_option(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

/// ポート番号の文字列をパースする
fn parse_port(value: &str, url: &str) -> Result<u16, MoqError> {
    value
        .parse()
        .map_err(|_| MoqError::Url(format!("invalid port '{value}': {url}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ホストとポートを指定した URL をパースできること
    #[test]
    fn test_parse_host_with_port() {
        let server =
            ServerUrl::parse("moqt://example.com:4443/live").expect("パースに成功すること");
        assert_eq!(server.host(), "example.com");
        assert_eq!(server.port(), 4443);
        assert_eq!(server.authority(), "example.com:4443");
        assert_eq!(server.path_option(), Some("/live"));
    }

    /// ポートを省略した場合はデフォルトポートを使うこと
    #[test]
    fn test_parse_default_port() {
        let server = ServerUrl::parse("moqt://example.com").expect("パースに成功すること");
        assert_eq!(server.port(), DEFAULT_PORT);
        assert_eq!(server.path_option(), None);
    }

    /// IPv6 リテラルをパースできること
    #[test]
    fn test_parse_ipv6_literal() {
        let server = ServerUrl::parse("moqt://[::1]:4443/live").expect("パースに成功すること");
        assert_eq!(server.host(), "::1");
        assert_eq!(server.port(), 4443);
        assert_eq!(server.path_option(), Some("/live"));
    }

    /// パスが `/` 単体の場合は PATH を送らないこと
    #[test]
    fn test_parse_root_path_is_omitted() {
        let server = ServerUrl::parse("moqt://example.com:4443/").expect("パースに成功すること");
        assert_eq!(server.path_option(), None);
    }

    /// moqt 以外のスキームは拒否すること
    #[test]
    fn test_parse_rejects_other_scheme() {
        assert!(ServerUrl::parse("https://example.com/live").is_err());
    }

    /// 不正なポートは拒否すること
    #[test]
    fn test_parse_rejects_invalid_port() {
        assert!(ServerUrl::parse("moqt://example.com:not-a-port/live").is_err());
    }
}
