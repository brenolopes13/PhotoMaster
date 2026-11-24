//! Contas de nuvem. Cada provedor só precisa listar os arquivos de mídia
//! com tamanho e hashes; a conferência de backup é feita localmente.

pub mod gdrive;
pub mod oauth;
pub mod onedrive;
pub mod webdav;

use crate::db::{Account, Db};
use crate::Progress;
use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum CloudConfig {
    Nextcloud {
        url: String,
        user: String,
        /// Senha de aplicativo (Configurações → Segurança no Nextcloud).
        password: String,
    },
    Onedrive {
        client_id: String,
        refresh_token: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        token_url: Option<String>,
    },
    Gdrive {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        token_url: Option<String>,
    },
}

impl CloudConfig {
    pub fn provider(&self) -> &'static str {
        match self {
            CloudConfig::Nextcloud { .. } => "nextcloud",
            CloudConfig::Onedrive { .. } => "onedrive",
            CloudConfig::Gdrive { .. } => "gdrive",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub quickxor: Option<String>,
}

/// Mensagem legível para erros HTTP do ureq (inclui o corpo da resposta).
pub(crate) fn http_err(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body: String = body.chars().take(300).collect();
            match code {
                401 | 403 => anyhow::anyhow!("acesso negado (HTTP {code}). Confira usuário, senha ou permissões. {body}"),
                _ => anyhow::anyhow!("o servidor respondeu HTTP {code}: {body}"),
            }
        }
        other => anyhow::anyhow!("falha de conexão: {other}"),
    }
}

pub fn add_account(db: &Db, label: &str, config: &CloudConfig) -> Result<i64> {
    db.add_account(config.provider(), label, &serde_json::to_string(config)?)
}

/// Baixa a lista de arquivos de mídia da conta e substitui a que estava
/// guardada. Só leitura: nada é alterado na nuvem.
pub fn sync_account(db: &Db, account: &Account, progress: Progress) -> Result<usize> {
    let config: CloudConfig = serde_json::from_str(&account.config).context("configuração da conta inválida")?;
    progress(0, 0, &format!("Listando arquivos em {}…", account.label));
    let report = |n: usize| progress(n, 0, &format!("{n} arquivos encontrados em {}", account.label));
    let files = match &config {
        CloudConfig::Nextcloud { url, user, password } => webdav::list(url, user, password, &report)?,
        CloudConfig::Onedrive { client_id, refresh_token, api_base, token_url } => {
            let tok = oauth::refresh(oauth::Provider::Onedrive, client_id, None, refresh_token, token_url.as_deref())?;
            if let Some(new_refresh) = tok.refresh_token.filter(|r| r != refresh_token) {
                let updated = CloudConfig::Onedrive {
                    client_id: client_id.clone(),
                    refresh_token: new_refresh,
                    api_base: api_base.clone(),
                    token_url: token_url.clone(),
                };
                db.update_account_config(account.id, &serde_json::to_string(&updated)?)?;
            }
            onedrive::list(&tok.access_token, api_base.as_deref(), &report)?
        }
        CloudConfig::Gdrive { client_id, client_secret, refresh_token, api_base, token_url } => {
            let tok = oauth::refresh(oauth::Provider::Gdrive, client_id, Some(client_secret), refresh_token, token_url.as_deref())?;
            gdrive::list(&tok.access_token, api_base.as_deref(), &report)?
        }
    };

    let mut c = db.conn();
    let tx = c.transaction()?;
    tx.execute("DELETE FROM cloud_files WHERE account_id=?", [account.id])?;
    {
        let mut st = tx.prepare(
            "INSERT INTO cloud_files(account_id,path,name,size,md5,sha1,sha256,quickxor) VALUES(?,?,?,?,?,?,?,?)",
        )?;
        for f in &files {
            st.execute(params![account.id, f.path, f.name, f.size as i64, f.md5, f.sha1, f.sha256, f.quickxor])?;
        }
    }
    tx.execute("UPDATE cloud_accounts SET last_sync=? WHERE id=?", params![crate::now_secs(), account.id])?;
    tx.commit()?;
    Ok(files.len())
}
