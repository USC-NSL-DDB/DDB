use std::{collections::HashSet, fs, path::Path, sync::Arc};

use anyhow::{bail, Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use ddb_api_types::v2::{DdbErrorCode, PermissionScope};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    application::{ApplicationError, PrincipalContext},
    telemetry::{record_authorization, route_name},
};
use crate::common::Config;

const MAX_TOKEN_FILE_BYTES: u64 = 64 * 1024;
const MAX_TOKENS: usize = 64;
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 512;

#[derive(Clone)]
pub(crate) struct ApiAuthorization {
    mode: AuthorizationMode,
}

#[derive(Clone)]
enum AuthorizationMode {
    Locked,
    InsecureDevelopment,
    Bearer(Arc<Vec<TokenGrant>>),
}

#[derive(Clone)]
struct TokenGrant {
    digest: [u8; 32],
    principal_id: String,
    scope: PermissionScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenFile {
    tokens: Vec<TokenEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenEntry {
    token: String,
    scope: TokenScope,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TokenScope {
    Read,
    Control,
    Admin,
}

impl From<TokenScope> for PermissionScope {
    fn from(value: TokenScope) -> Self {
        match value {
            TokenScope::Read => Self::Read,
            TokenScope::Control => Self::Control,
            TokenScope::Admin => Self::Admin,
        }
    }
}

impl ApiAuthorization {
    pub(crate) fn from_config(config: &Config) -> Result<Arc<Self>> {
        let insecure = config.conf.api_insecure_allow_unauthenticated_v2;
        let token_file = config.conf.api_auth_token_file.as_deref();
        if insecure && token_file.is_some() {
            bail!(
                "Conf.api_auth_token_file and Conf.api_insecure_allow_unauthenticated_v2 are mutually exclusive"
            );
        }
        if insecure {
            return Ok(Arc::new(Self {
                mode: AuthorizationMode::InsecureDevelopment,
            }));
        }
        let Some(path) = token_file else {
            return Ok(Arc::new(Self {
                mode: AuthorizationMode::Locked,
            }));
        };
        Ok(Arc::new(Self {
            mode: AuthorizationMode::Bearer(Arc::new(load_token_file(Path::new(path))?)),
        }))
    }

    fn authenticate(
        &self,
        headers: &HeaderMap,
        required: PermissionScope,
    ) -> Result<PrincipalContext, ApplicationError> {
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        self.authenticate_authorization(authorization, required)
    }

    pub(crate) fn authenticate_authorization(
        &self,
        authorization: Option<&str>,
        required: PermissionScope,
    ) -> Result<PrincipalContext, ApplicationError> {
        match &self.mode {
            AuthorizationMode::InsecureDevelopment => {
                PrincipalContext::with_scope("insecure-development", PermissionScope::Admin)
            }
            AuthorizationMode::Locked => Err(ApplicationError::new(
                DdbErrorCode::Unauthenticated,
                "v2 API authentication is required but no token file is configured",
            )),
            AuthorizationMode::Bearer(grants) => {
                let token = bearer_token(authorization)?;
                let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
                let grant = grants
                    .iter()
                    .find(|grant| constant_time_eq(&grant.digest, &digest))
                    .ok_or_else(|| {
                        ApplicationError::new(
                            DdbErrorCode::Unauthenticated,
                            "bearer credentials are invalid",
                        )
                    })?;
                if !scope_allows(grant.scope, required) {
                    return Err(ApplicationError::new(
                        DdbErrorCode::PermissionDenied,
                        "the authenticated principal lacks the required API scope",
                    ));
                }
                PrincipalContext::with_scope(grant.principal_id.clone(), grant.scope)
            }
        }
    }

    #[cfg(test)]
    fn for_token(token: &str, scope: PermissionScope) -> Arc<Self> {
        Arc::new(Self {
            mode: AuthorizationMode::Bearer(Arc::new(vec![grant(token, scope)])),
        })
    }
}

pub(crate) async fn require_read(
    State(auth): State<Arc<ApiAuthorization>>,
    request: Request,
    next: Next,
) -> Response {
    authorize(auth, PermissionScope::Read, request, next).await
}

pub(crate) async fn require_control(
    State(auth): State<Arc<ApiAuthorization>>,
    request: Request,
    next: Next,
) -> Response {
    authorize(auth, PermissionScope::Control, request, next).await
}

pub(crate) async fn require_admin(
    State(auth): State<Arc<ApiAuthorization>>,
    request: Request,
    next: Next,
) -> Response {
    authorize(auth, PermissionScope::Admin, request, next).await
}

async fn authorize(
    auth: Arc<ApiAuthorization>,
    required: PermissionScope,
    mut request: Request,
    next: Next,
) -> Response {
    let method = route_name(&request).to_string();
    match auth.authenticate(request.headers(), required) {
        Ok(principal) => {
            record_authorization("http", &method, required, "allowed", Some(principal.id()));
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(error) => {
            record_authorization("http", &method, required, "denied", None);
            authorization_error(error)
        }
    }
}

fn authorization_error(error: ApplicationError) -> Response {
    let status = match error.code() {
        DdbErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
        _ => StatusCode::UNAUTHORIZED,
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut response = (status, Json(error.to_contract(request_id))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer realm=\"ddb-v2\""),
        );
    }
    response
}

fn bearer_token(authorization: Option<&str>) -> Result<&str, ApplicationError> {
    let value = authorization.ok_or_else(|| {
        ApplicationError::new(
            DdbErrorCode::Unauthenticated,
            "bearer credentials are required",
        )
    })?;
    let token = value.strip_prefix("Bearer ").ok_or_else(|| {
        ApplicationError::new(
            DdbErrorCode::Unauthenticated,
            "authorization scheme must be Bearer",
        )
    })?;
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(ApplicationError::new(
            DdbErrorCode::Unauthenticated,
            "bearer credentials are invalid",
        ));
    }
    Ok(token)
}

fn load_token_file(path: &Path) -> Result<Vec<TokenGrant>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect API token file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("API token path {} is not a regular file", path.display());
    }
    if metadata.len() > MAX_TOKEN_FILE_BYTES {
        bail!(
            "API token file {} exceeds {} bytes",
            path.display(),
            MAX_TOKEN_FILE_BYTES
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "API token file {} must not be accessible by group or other users",
                path.display()
            );
        }
    }

    let bytes = fs::read(path)
        .with_context(|| format!("failed to read API token file {}", path.display()))?;
    let file: TokenFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse API token file {}", path.display()))?;
    if file.tokens.is_empty() || file.tokens.len() > MAX_TOKENS {
        bail!("API token file must contain between 1 and {MAX_TOKENS} tokens");
    }

    let mut seen = HashSet::new();
    let mut grants = Vec::with_capacity(file.tokens.len());
    for entry in file.tokens {
        if entry.token.len() < MIN_TOKEN_BYTES || entry.token.len() > MAX_TOKEN_BYTES {
            bail!(
                "each API bearer token must contain between {MIN_TOKEN_BYTES} and {MAX_TOKEN_BYTES} bytes"
            );
        }
        let grant = grant(&entry.token, entry.scope.into());
        if !seen.insert(grant.digest) {
            bail!("API token file contains a duplicate token");
        }
        grants.push(grant);
    }
    Ok(grants)
}

fn grant(token: &str, scope: PermissionScope) -> TokenGrant {
    let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let principal_id = format!(
        "token_{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    TokenGrant {
        digest,
        principal_id,
        scope,
    }
}

fn scope_allows(granted: PermissionScope, required: PermissionScope) -> bool {
    match granted {
        PermissionScope::Admin => true,
        PermissionScope::Control => {
            matches!(required, PermissionScope::Read | PermissionScope::Control)
        }
        PermissionScope::Read => required == PermissionScope::Read,
        PermissionScope::Unspecified => false,
    }
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn bearer_scopes_are_hierarchical_and_principal_is_not_the_token() {
        let token = "0123456789abcdef0123456789abcdef";
        let auth = ApiAuthorization::for_token(token, PermissionScope::Control);
        let principal = auth
            .authenticate(&headers(token), PermissionScope::Read)
            .unwrap();
        assert!(principal.id().starts_with("token_"));
        assert_ne!(principal.id(), token);
        assert!(auth
            .authenticate(&headers(token), PermissionScope::Control)
            .is_ok());
        assert_eq!(
            auth.authenticate(&headers(token), PermissionScope::Admin)
                .unwrap_err()
                .code(),
            DdbErrorCode::PermissionDenied
        );
    }

    #[test]
    fn invalid_or_missing_bearer_credentials_are_indistinguishable() {
        let token = "0123456789abcdef0123456789abcdef";
        let auth = ApiAuthorization::for_token(token, PermissionScope::Read);
        assert_eq!(
            auth.authenticate(&HeaderMap::new(), PermissionScope::Read)
                .unwrap_err()
                .code(),
            DdbErrorCode::Unauthenticated
        );
        assert_eq!(
            auth.authenticate(
                &headers("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
                PermissionScope::Read
            )
            .unwrap_err()
            .code(),
            DdbErrorCode::Unauthenticated
        );
    }

    #[test]
    fn constant_time_comparison_checks_all_digest_bytes() {
        let left = [7_u8; 32];
        let mut right = left;
        assert!(constant_time_eq(&left, &right));
        right[31] ^= 1;
        assert!(!constant_time_eq(&left, &right));
    }
}
