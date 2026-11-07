use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Error;

pub(super) const DEFAULT_PORT: u16 = 18080;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Config {
    pub enabled: bool,
    pub port: u16,
    pub public_origin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            public_origin: None,
        }
    }
}

impl Config {
    pub fn load(state: &Path) -> Result<Self, Error> {
        let text = match std::fs::read_to_string(state.join("webserve.toml")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut cfg: Self = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&mut self) -> Result<(), Error> {
        if self.port == 0 {
            return Err(Error::Invalid("Web端口必须为1..65535"));
        }
        if let Some(origin) = &self.public_origin {
            let mut parsed = url::Url::parse(origin)
                .map_err(|_| Error::Invalid("公开origin须为https://域名[:端口]"))?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(Error::Invalid(
                    "公开origin须为无路径/用户信息的HTTPS origin",
                ));
            }
            parsed.set_path("");
            self.public_origin = Some(parsed.origin().ascii_serialization());
        }
        Ok(())
    }

    pub fn save(&self, state: &Path) -> Result<(), Error> {
        super::credential::atomic_write(
            &state.join("webserve.toml"),
            toml::to_string(self)?.as_bytes(),
        )
    }

    pub fn permits_host(&self, value: &str) -> bool {
        match super::http::check_authority(value, self.port) {
            super::http::AuthorityCheck::Invalid => return false,
            super::http::AuthorityCheck::Local => return true,
            super::http::AuthorityCheck::PublicCandidate => {}
        }
        let Some(public) = &self.public_origin else {
            return false;
        };
        let Ok(host) = url::Url::parse(&format!("https://{value}")) else {
            return false;
        };
        host.username().is_empty()
            && host.password().is_none()
            && host.path() == "/"
            && host.query().is_none()
            && host.fragment().is_none()
            && host.origin().ascii_serialization() == *public
    }

    pub fn permits_origin(&self, value: &str) -> bool {
        let Ok(origin) = url::Url::parse(value) else {
            return false;
        };
        if !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return false;
        }
        let normalized = origin.origin().ascii_serialization();
        normalized
            == url::Url::parse(&format!("http://127.0.0.1:{}", self.port))
                .expect("固定loopback URL")
                .origin()
                .ascii_serialization()
            || normalized
                == url::Url::parse(&format!("http://localhost:{}", self.port))
                    .expect("固定loopback URL")
                    .origin()
                    .ascii_serialization()
            || self.public_origin.as_ref() == Some(&normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_normalization_and_exact_authority() {
        let mut c = Config {
            public_origin: Some("https://EXAMPLE.com:443".into()),
            ..Config::default()
        };
        c.validate().unwrap();
        assert!(c.permits_origin("https://example.com"));
        assert!(c.permits_host("example.com:443"));
        assert!(!c.permits_host("example.com:444"));
        assert!(!c.permits_origin("https://example.com.evil"));
        assert!(!c.permits_host("user@example.com"));
        assert!(!c.permits_host("user@127.0.0.1:18080"));
        assert!(!c.permits_host("user@localhost:18080"));
        assert!(!c.permits_origin("https://example.com/path"));
    }
}
