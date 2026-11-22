//! The network gate: every outbound HTTP request and socket a component
//! makes is recorded here and checked against that component's grants. A
//! granted request is answered from the test's `http` stubs; nothing reaches
//! a network.
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::{RequestOptions, WasiBody, WasiHttpHooks};

use crate::host::Host;
use crate::record::{Egress, Refusal, Refused};

/// Whether `host` (and `authority`, with its port) is covered by a grant: an
/// exact host, a host with port, or `*.example.com` for any subdomain.
pub fn granted(grants: &[String], host: &str, authority: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let authority = authority.to_ascii_lowercase();
    grants.iter().any(|g| {
        let g = g.to_ascii_lowercase();
        match g.strip_prefix("*.") {
            Some(suffix) => host.ends_with(&format!(".{suffix}")),
            None => g == host || g == authority,
        }
    })
}

pub struct Gate {
    /// Who is asking: `agent` or a tool component's name.
    pub by: String,
    pub hosts: Vec<String>,
    pub host: Arc<Host>,
}

type HttpResult<T> = Result<T, wasmtime_wasi_http::Error>;
type HttpDone = Box<dyn Future<Output = HttpResult<()>> + Send>;

impl WasiHttpHooks for Gate {
    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        _options: Option<RequestOptions>,
        _completion: HttpDone,
    ) -> Box<dyn Future<Output = HttpResult<(http::Response<WasiBody>, HttpDone)>> + Send> {
        let uri = request.uri();
        let url = uri.to_string();
        let host = uri.host().unwrap_or_default().to_string();
        let authority = uri.authority().map(|a| a.to_string()).unwrap_or_default();
        let method = request.method().to_string();
        let allowed = granted(&self.hosts, &host, &authority);
        let mut rec = self.host.record();
        rec.egress.push(Egress {
            by: self.by.clone(),
            method: method.clone(),
            url: url.clone(),
            host: host.clone(),
            allowed,
        });
        if !allowed {
            rec.refused.push(Refused {
                by: self.by.clone(),
                what: Refusal::Host { host, url },
            });
            return Box::new(async { Err(ErrorCode::HttpRequestDenied.into()) });
        }
        let stub = self
            .host
            .http
            .iter()
            .find(|s| s.method.eq_ignore_ascii_case(&method) && same_url(&s.url, &url))
            .cloned();
        let Some(stub) = stub else {
            rec.problems.push(format!(
                "{} sent {method} {url}, which the grants allow, but the test has no `http` stub for it",
                self.by
            ));
            return Box::new(async { Err(ErrorCode::DestinationNotFound.into()) });
        };
        Box::new(async move {
            let body = Full::new(Bytes::from(stub.body))
                .map_err(|never| match never {})
                .boxed_unsync();
            let response = http::Response::builder()
                .status(stub.status)
                .body(body)
                .map_err(|_| wasmtime_wasi_http::Error::from(ErrorCode::InternalError(None)))?;
            Ok((response, Box::new(async { Ok(()) }) as HttpDone))
        })
    }
}

/// URLs compare without a trailing slash on an empty path.
fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::granted;

    #[test]
    fn matches_hosts() {
        let g = vec![
            "api.example.com".to_string(),
            "*.cdn.test".to_string(),
            "local:8080".to_string(),
        ];
        assert!(granted(&g, "api.example.com", "api.example.com"));
        assert!(granted(&g, "API.example.com", "API.example.com:443"));
        assert!(!granted(&g, "evil.example.com", "evil.example.com"));
        assert!(!granted(&g, "api.example.com.evil", "api.example.com.evil"));
        assert!(granted(&g, "a.cdn.test", "a.cdn.test"));
        assert!(!granted(&g, "cdn.test", "cdn.test"));
        assert!(granted(&g, "local", "local:8080"));
        assert!(!granted(&g, "local", "local:9090"));
        assert!(!granted(&[], "api.example.com", "api.example.com"));
    }
}
