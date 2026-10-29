//! Who a request comes from: the Operator, by a trusted proxy's identity header or by the Operator token, else no one.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use title_select_store::config;

use crate::StartError;
use crate::routes::error_response;

/// The identity a request was authenticated as, recorded as the actor of the Jobs it starts.
#[derive(Debug, Clone)]
pub struct Actor(pub String);

/// The actor recorded for a request authenticated by the Operator token.
pub const TOKEN_ACTOR: &str = "Operator token";

#[derive(Debug)]
pub struct Auth {
    identity_header: Option<HeaderName>,
    trusted_proxies: Vec<IpNet>,
    /// Only the token's SHA-256 is kept.
    token_sha256: Option<[u8; 32]>,
    dev_identity: Option<String>,
}

impl Auth {
    /// Refuses a Service with no way to authenticate the Operator, or with a `dev_identity` off loopback.
    pub fn new(
        config: &config::Auth,
        token_file: Option<&Path>,
        listen: SocketAddr,
    ) -> Result<Auth, StartError> {
        let identity_header = config
            .identity_header
            .as_deref()
            .map(|h| {
                HeaderName::try_from(h)
                    .map_err(|e| StartError::Auth(format!("auth.identity_header {h:?}: {e}")))
            })
            .transpose()?;
        let token_sha256 = token_file.map(read_token).transpose()?;
        if identity_header.is_none() && token_sha256.is_none() && config.dev_identity.is_none() {
            return Err(StartError::Auth(
                "no way to authenticate the Operator: set [auth] identity_header and trusted_proxies, or \
                 TITLE_SELECT_OPERATOR_TOKEN_FILE, or [auth] dev_identity on a loopback address"
                    .into(),
            ));
        }
        if config.dev_identity.is_some() && !listen.ip().is_loopback() {
            return Err(StartError::Auth(format!(
                "auth.dev_identity is only allowed on a loopback listen address, not {listen}"
            )));
        }
        Ok(Auth {
            identity_header,
            trusted_proxies: config.trusted_proxies.clone(),
            token_sha256,
            dev_identity: config.dev_identity.clone(),
        })
    }

    fn authenticate(&self, peer: IpAddr, headers: &HeaderMap) -> Result<Actor, &'static str> {
        if let Some(name) = &self.identity_header
            && let Some(value) = headers.get(name)
        {
            let peer = peer.to_canonical();
            if !self.trusted_proxies.iter().any(|net| net.contains(&peer)) {
                return Err("the identity header is only trusted from a trusted proxy");
            }
            return match value.to_str().map(str::trim) {
                Ok(identity) if !identity.is_empty() => Ok(Actor(identity.to_owned())),
                _ => Err("the identity header is empty or not text"),
            };
        }
        if let Some(presented) = headers.get(AUTHORIZATION) {
            let Some(expected) = &self.token_sha256 else {
                return Err("the Operator token is not enabled");
            };
            let token = presented
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default();
            let presented: [u8; 32] = Sha256::digest(token.trim().as_bytes()).into();
            return if bool::from(presented.ct_eq(expected)) {
                Ok(Actor(TOKEN_ACTOR.into()))
            } else {
                Err("wrong Operator token")
            };
        }
        match &self.dev_identity {
            Some(identity) => Ok(Actor(identity.clone())),
            None => Err("authentication required"),
        }
    }
}

fn read_token(path: &Path) -> Result<[u8; 32], StartError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| StartError::Auth(format!("Operator token file {}: {e}", path.display())))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(StartError::Auth(format!(
            "Operator token file {} is empty",
            path.display()
        )));
    }
    Ok(Sha256::digest(token.as_bytes()).into())
}

/// Rejects every request that does not authenticate as the Operator, and records the actor of those that do.
pub async fn require_operator(
    State(auth): State<std::sync::Arc<Auth>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    match auth.authenticate(peer.ip(), request.headers()) {
        Ok(actor) => {
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Err(reason) => {
            let mut response = error_response(StatusCode::UNAUTHORIZED, reason.into(), None);
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, "Bearer".parse().unwrap());
            response.into_response()
        }
    }
}
