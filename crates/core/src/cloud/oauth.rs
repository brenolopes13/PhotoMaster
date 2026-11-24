//! OAuth 2.0 (authorization code + PKCE) para OneDrive e Google Drive.
//!
//! O app não embute credenciais: o usuário registra o próprio aplicativo na
//! Microsoft/Google e informa o client ID. Pedimos só permissão de leitura.

use super::http_err;
use anyhow::{bail, Result};
use base64::Engine;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Onedrive,
    Gdrive,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "onedrive" => Some(Provider::Onedrive),
            "gdrive" => Some(Provider::Gdrive),
            _ => None,
        }
    }

    fn auth_url(self) -> &'static str {
        match self {
            Provider::Onedrive => "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            Provider::Gdrive => "https://accounts.google.com/o/oauth2/v2/auth",
        }
    }

    fn token_url(self) -> &'static str {
        match self {
            Provider::Onedrive => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            Provider::Gdrive => "https://oauth2.googleapis.com/token",
        }
    }

    /// Escopos mínimos: apenas leitura (no Google, apenas metadados).
    pub fn scope(self) -> &'static str {
        match self {
            Provider::Onedrive => "Files.Read offline_access",
            Provider::Gdrive => "https://www.googleapis.com/auth/drive.metadata.readonly",
        }
    }
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("gerador de números aleatórios do sistema");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn pkce() -> Pkce {
    let verifier = random_token();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

pub fn authorize_url(provider: Provider, client_id: &str, redirect: &str, state: &str, challenge: &str) -> String {
    let mut url = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.auth_url(),
        enc(client_id),
        enc(redirect),
        enc(provider.scope()),
        enc(state),
        enc(challenge),
    );
    if provider == Provider::Gdrive {
        // Necessário para o Google devolver um refresh token.
        url.push_str("&access_type=offline&prompt=consent");
    }
    url
}

#[derive(Debug)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

fn token_request(provider: Provider, token_url: Option<&str>, form: &[(&str, &str)]) -> Result<Tokens> {
    let url = token_url.unwrap_or(provider.token_url());
    let v: serde_json::Value = ureq::post(url).send_form(form).map_err(http_err)?.into_json()?;
    let Some(access) = v["access_token"].as_str() else {
        bail!("resposta sem access_token: {v}");
    };
    Ok(Tokens { access_token: access.into(), refresh_token: v["refresh_token"].as_str().map(String::from) })
}

pub fn exchange_code(
    provider: Provider,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    verifier: &str,
    redirect: &str,
    token_url: Option<&str>,
) -> Result<Tokens> {
    let mut form = vec![
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect),
    ];
    if let Some(s) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", s));
    }
    token_request(provider, token_url, &form)
}

pub fn refresh(
    provider: Provider,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
    token_url: Option<&str>,
) -> Result<Tokens> {
    let mut form = vec![
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if let Some(s) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", s));
    }
    token_request(provider, token_url, &form)
}
