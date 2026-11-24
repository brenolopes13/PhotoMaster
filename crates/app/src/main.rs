//! PhotoMaster desktop: servidor HTTP local (só 127.0.0.1) + interface web.

use anyhow::Context;
use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use clap::Parser;
use photomaster_core::cloud::{self, oauth, CloudConfig};
use photomaster_core::db::Db;
use photomaster_core::{adb, analysis, enhance, media, optimize, scan, trash, Dirs, Progress};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Parser)]
#[command(name = "photomaster", version, about = "Gerencie fotos e vídeos do celular: backup, duplicadas, otimização e melhoria.")]
struct Cli {
    /// Pasta onde ficam o índice, as miniaturas, a lixeira e os espelhos.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Porta local da interface.
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Não abrir o navegador automaticamente.
    #[arg(long)]
    no_browser: bool,
}

#[derive(Serialize, Clone, Default)]
struct Job {
    seq: u64,
    running: bool,
    kind: String,
    message: String,
    done: usize,
    total: usize,
    error: Option<String>,
    result: Option<Value>,
}

struct PendingOAuth {
    provider: oauth::Provider,
    client_id: String,
    client_secret: String,
    label: String,
    state: String,
    verifier: String,
}

struct App {
    db: Db,
    dirs: Dirs,
    port: u16,
    job: Mutex<Job>,
    oauth: Mutex<Option<PendingOAuth>>,
}

type S = State<Arc<App>>;

impl App {
    fn job(&self) -> MutexGuard<'_, Job> {
        self.job.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn redirect_uri(&self) -> String {
        format!("http://localhost:{}/oauth/callback", self.port)
    }
}

// ---- erros -----------------------------------------------------------------

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type Api<T = Value> = Result<Json<T>, ApiError>;

/// Roda trabalho bloqueante (SQLite, disco) fora do executor assíncrono.
async fn blocking<T, F>(app: &Arc<App>, f: F) -> Api<T>
where
    T: Send + 'static,
    F: FnOnce(&App) -> anyhow::Result<T> + Send + 'static,
{
    let app = app.clone();
    let r = tokio::task::spawn_blocking(move || f(&app))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(r?))
}

// ---- tarefas em segundo plano ---------------------------------------------

/// Inicia a (única) tarefa longa. Recusa se já houver uma rodando.
fn start_job<F>(app: &Arc<App>, kind: &str, f: F) -> Result<Json<Value>, ApiError>
where
    F: FnOnce(&App, Progress) -> anyhow::Result<Value> + Send + 'static,
{
    {
        let mut j = app.job();
        if j.running {
            return Err(ApiError(StatusCode::CONFLICT, format!("Aguarde: “{}” ainda está em andamento.", j.kind)));
        }
        *j = Job { seq: j.seq + 1, running: true, kind: kind.into(), message: "Iniciando…".into(), ..Default::default() };
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let progress = |done: usize, total: usize, msg: &str| {
            let mut j = app.job();
            j.done = done;
            j.total = total;
            j.message = msg.to_string();
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&app, &progress)));
        let mut j = app.job();
        j.running = false;
        match r {
            Ok(Ok(v)) => j.result = Some(v),
            Ok(Err(e)) => j.error = Some(format!("{e:#}")),
            Err(_) => j.error = Some("erro interno inesperado".into()),
        }
    });
    Ok(Json(json!({ "started": true })))
}

fn scan_all(app: &App, progress: Progress) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    for s in app.db.sources()? {
        let label = s.label.clone();
        let p = |d: usize, t: usize, m: &str| progress(d, t, &format!("{label}: {m}"));
        match scan::scan_source(&app.db, &app.dirs, &s, &p) {
            Ok(st) => out.push(json!({ "source": s.label, "stats": st })),
            Err(e) => out.push(json!({ "source": s.label, "error": e.to_string() })),
        }
    }
    Ok(json!({ "scans": out }))
}

// ---- handlers: estado, fontes, varredura ------------------------------------

async fn state(State(app): S) -> Api {
    blocking(&app, |app| {
        Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "sources": app.db.sources()?,
            "accounts": app.db.accounts()?,
            "overview": analysis::overview(&app.db)?,
            "ffmpeg": media::ffmpeg_available(),
            "data_dir": app.dirs.data,
            "redirect_uri": app.redirect_uri(),
        }))
    })
    .await
}

async fn job(State(app): S) -> Json<Job> {
    Json(app.job().clone())
}

#[derive(Deserialize)]
struct AddSource {
    path: String,
    label: Option<String>,
}

async fn add_source(State(app): S, Json(b): Json<AddSource>) -> Api {
    let path = std::fs::canonicalize(b.path.trim()).map_err(|_| bad("Pasta não encontrada."))?;
    if !path.is_dir() {
        return Err(bad("O caminho informado não é uma pasta."));
    }
    if path.starts_with(&app.dirs.data) {
        return Err(bad("Esta pasta é usada internamente pelo PhotoMaster."));
    }
    // No Windows, canonicalize devolve o prefixo \\?\, que atrapalha a leitura.
    let shown = path.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let label = b.label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| shown.clone())
    });
    blocking(&app, move |app| Ok(json!(app.db.add_source("folder", &label, &shown, None)?))).await
}

async fn remove_source(State(app): S, AxPath(id): AxPath<i64>) -> Api {
    blocking(&app, move |app| {
        app.db.remove_source(id)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn start_scan(State(app): S) -> Api {
    start_job(&app, "Análise", scan_all)
}

#[derive(Deserialize)]
struct BrowseQ {
    path: Option<String>,
}

/// Navegador de pastas do lado do servidor (o navegador web não revela
/// caminhos reais do disco).
async fn browse(State(app): S, Query(q): Query<BrowseQ>) -> Api {
    blocking(&app, move |_| {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let path = q.path.filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.clone());
        let mut entries: Vec<Value> = std::fs::read_dir(&path)
            .with_context(|| format!("não foi possível abrir {}", path.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| json!({ "name": e.file_name().to_string_lossy(), "path": e.path() }))
            .collect();
        entries.sort_by_key(|e| e["name"].as_str().unwrap_or("").to_lowercase());

        let mut shortcuts = vec![json!({ "name": "Pasta pessoal", "path": home })];
        if let Some(p) = dirs::picture_dir() {
            shortcuts.push(json!({ "name": "Imagens", "path": p }));
        }
        #[cfg(windows)]
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            if std::path::Path::new(&drive).exists() {
                shortcuts.push(json!({ "name": drive, "path": drive }));
            }
        }
        #[cfg(unix)]
        {
            // Onde o Linux monta celulares (MTP/iPhone via gvfs) e pendrives.
            let user = std::env::var("USER").unwrap_or_default();
            let candidates = [
                dirs::runtime_dir().map(|d| d.join("gvfs")),
                Some(PathBuf::from(format!("/media/{user}"))),
                Some(PathBuf::from(format!("/run/media/{user}"))),
            ];
            for dir in candidates.into_iter().flatten() {
                for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    shortcuts.push(json!({ "name": format!("Dispositivo: {}", e.file_name().to_string_lossy()), "path": e.path() }));
                }
            }
        }
        Ok(json!({ "path": path, "parent": path.parent(), "dirs": entries, "shortcuts": shortcuts }))
    })
    .await
}

// ---- handlers: Android -------------------------------------------------------

async fn adb_devices(State(app): S) -> Api {
    blocking(&app, |_| Ok(json!(adb::devices()?))).await
}

#[derive(Deserialize)]
struct AdbImport {
    serial: String,
    label: Option<String>,
}

async fn adb_import(State(app): S, Json(b): Json<AdbImport>) -> Api {
    start_job(&app, "Importação do Android", move |app, progress| {
        let label = b.label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| format!("Android {}", b.serial));
        let source = adb::import(&app.db, &app.dirs, &b.serial, &label, progress)?;
        let stats = scan::scan_source(&app.db, &app.dirs, &source, progress)?;
        Ok(json!({ "scans": [{ "source": label, "stats": stats }] }))
    })
}

// ---- handlers: análises ------------------------------------------------------

#[derive(Deserialize)]
struct NumQ {
    value: Option<f64>,
}

async fn duplicates(State(app): S, Query(q): Query<NumQ>) -> Api {
    let t = q.value.unwrap_or(6.0).clamp(0.0, 16.0) as u32;
    blocking(&app, move |app| Ok(json!(analysis::duplicates(&app.db, t)?))).await
}

async fn short_videos(State(app): S, Query(q): Query<NumQ>) -> Api {
    let max = q.value.unwrap_or(3.0).clamp(0.0, 600.0);
    blocking(&app, move |app| Ok(json!(analysis::short_videos(&app.db, max)?))).await
}

async fn blurry(State(app): S, Query(q): Query<NumQ>) -> Api {
    let t = q.value.unwrap_or(40.0).clamp(0.0, 10_000.0);
    blocking(&app, move |app| Ok(json!(analysis::blurry(&app.db, t)?))).await
}

async fn optimize_list(State(app): S) -> Api {
    blocking(&app, |app| Ok(json!(analysis::optimize_candidates(&app.db)?))).await
}

async fn backup(State(app): S) -> Api {
    blocking(&app, |app| Ok(json!(analysis::backup_report(&app.db)?))).await
}

async fn library(State(app): S) -> Api {
    blocking(&app, |app| Ok(json!(app.db.media_where("error IS NULL ORDER BY mtime DESC")?))).await
}

// ---- handlers: ações ---------------------------------------------------------

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

async fn trash_add(State(app): S, Json(b): Json<Ids>) -> Api {
    blocking(&app, move |app| Ok(json!({ "results": trash::trash_media(&app.db, &app.dirs, &b.ids, "excluído") }))).await
}

async fn trash_list(State(app): S) -> Api {
    blocking(&app, |app| Ok(json!(app.db.trash_items()?))).await
}

async fn trash_restore(State(app): S, Json(b): Json<Ids>) -> Api {
    blocking(&app, move |app| Ok(json!({ "results": trash::restore(&app.db, &app.dirs, &b.ids) }))).await
}

async fn trash_empty(State(app): S) -> Api {
    blocking(&app, |app| Ok(json!({ "freed": trash::empty(&app.db)? }))).await
}

#[derive(Deserialize)]
struct OptimizeReq {
    ids: Vec<i64>,
    #[serde(default)]
    options: optimize::OptimizeOptions,
}

async fn optimize_run(State(app): S, Json(b): Json<OptimizeReq>) -> Api {
    start_job(&app, "Otimização", move |app, progress| {
        Ok(json!({ "results": optimize::optimize(&app.db, &app.dirs, &b.ids, &b.options, progress) }))
    })
}

#[derive(Deserialize)]
struct EnhanceReq {
    ids: Vec<i64>,
    #[serde(default)]
    options: enhance::EnhanceOptions,
}

async fn enhance_run(State(app): S, Json(b): Json<EnhanceReq>) -> Api {
    start_job(&app, "Melhoria", move |app, progress| {
        Ok(json!({ "results": enhance::enhance(&app.db, &app.dirs, &b.ids, &b.options, progress) }))
    })
}

// ---- handlers: arquivos ------------------------------------------------------

async fn serve_path(path: PathBuf, req: Request) -> Response {
    use tower::ServiceExt;
    match tower_http::services::ServeFile::new(path).oneshot(req).await {
        Ok(r) => r.map(axum::body::Body::new),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn thumb(State(app): S, AxPath(sha1): AxPath<String>, req: Request) -> Response {
    if sha1.len() != 40 || !sha1.bytes().all(|b| b.is_ascii_hexdigit()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    serve_path(scan::thumb_path(&app.dirs, &sha1), req).await
}

/// Serve o arquivo original para pré-visualização. Só por id do índice:
/// nenhum caminho vem do cliente.
async fn file(State(app): S, AxPath(id): AxPath<i64>, req: Request) -> Response {
    match app.db.media(id) {
        Ok(Some(m)) => serve_path(PathBuf::from(m.path), req).await,
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---- handlers: nuvem ---------------------------------------------------------

fn sync_accounts(app: &App, ids: Vec<i64>, progress: Progress) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    for id in ids {
        let Some(acc) = app.db.account(id)? else { continue };
        match cloud::sync_account(&app.db, &acc, progress) {
            Ok(n) => out.push(json!({ "account": acc.label, "files": n })),
            Err(e) => out.push(json!({ "account": acc.label, "error": format!("{e:#}") })),
        }
    }
    Ok(json!({ "syncs": out }))
}

#[derive(Deserialize)]
struct NextcloudReq {
    label: Option<String>,
    url: String,
    user: String,
    password: String,
}

async fn cloud_add_nextcloud(State(app): S, Json(b): Json<NextcloudReq>) -> Api {
    if b.url.trim().is_empty() || b.user.trim().is_empty() || b.password.is_empty() {
        return Err(bad("Preencha endereço, usuário e senha de aplicativo."));
    }
    start_job(&app, "Conexão com Nextcloud", move |app, progress| {
        let label = b.label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| "Nextcloud".into());
        let config = CloudConfig::Nextcloud { url: b.url.trim().into(), user: b.user.trim().into(), password: b.password };
        let id = cloud::add_account(&app.db, &label, &config)?;
        let acc = app.db.account(id)?.context("conta recém-criada sumiu")?;
        // Se a primeira listagem falhar, a conta não fica salva pela metade.
        match cloud::sync_account(&app.db, &acc, progress) {
            Ok(n) => Ok(json!({ "syncs": [{ "account": label, "files": n }] })),
            Err(e) => {
                app.db.remove_account(id)?;
                Err(e)
            }
        }
    })
}

#[derive(Deserialize)]
struct OAuthStart {
    provider: String,
    client_id: String,
    #[serde(default)]
    client_secret: String,
    label: Option<String>,
}

async fn oauth_start(State(app): S, Json(b): Json<OAuthStart>) -> Api {
    let provider = oauth::Provider::parse(&b.provider).ok_or_else(|| bad("Provedor desconhecido."))?;
    let client_id = b.client_id.trim().to_string();
    if client_id.is_empty() {
        return Err(bad("Informe o Client ID."));
    }
    if provider == oauth::Provider::Gdrive && b.client_secret.trim().is_empty() {
        return Err(bad("O Google exige também o Client Secret do app de desktop."));
    }
    let pkce = oauth::pkce();
    let state = oauth::random_token();
    let url = oauth::authorize_url(provider, &client_id, &app.redirect_uri(), &state, &pkce.challenge);
    let default_label = if provider == oauth::Provider::Gdrive { "Google Drive" } else { "OneDrive" };
    *app.oauth.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingOAuth {
        provider,
        client_id,
        client_secret: b.client_secret.trim().to_string(),
        label: b.label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| default_label.into()),
        state,
        verifier: pkce.verifier,
    });
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct OAuthCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn page(title: &str, body: &str) -> Html<String> {
    // Texto vindo de fora é escapado antes de entrar no HTML.
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>PhotoMaster</title>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:15vh auto;padding:0 1rem\">\
         <h2>{}</h2><p>{}</p><p><a href=\"/#backup\">Voltar ao PhotoMaster</a></p>",
        esc(title),
        esc(body)
    ))
}

async fn oauth_callback(State(app): S, Query(q): Query<OAuthCallback>) -> Html<String> {
    if let Some(e) = q.error {
        return page("Autorização não concluída", &format!("{e}: {}", q.error_description.unwrap_or_default()));
    }
    let pending = app.oauth.lock().unwrap_or_else(|e| e.into_inner()).take();
    let (Some(p), Some(code)) = (pending, q.code) else {
        return page("Autorização inválida", "Não há pedido de conexão em andamento. Tente de novo pelo PhotoMaster.");
    };
    if q.state.as_deref() != Some(p.state.as_str()) {
        return page("Autorização inválida", "O código de verificação não confere. Tente de novo pelo PhotoMaster.");
    }
    let redirect = app.redirect_uri();
    let app2 = app.clone();
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
        let secret = Some(p.client_secret.as_str()).filter(|s| !s.is_empty());
        let tok = oauth::exchange_code(p.provider, &p.client_id, secret, &code, &p.verifier, &redirect, None)?;
        let refresh_token = tok.refresh_token.context("o provedor não devolveu um refresh token")?;
        let config = match p.provider {
            oauth::Provider::Onedrive => CloudConfig::Onedrive { client_id: p.client_id, refresh_token, api_base: None, token_url: None },
            oauth::Provider::Gdrive => CloudConfig::Gdrive {
                client_id: p.client_id,
                client_secret: p.client_secret,
                refresh_token,
                api_base: None,
                token_url: None,
            },
        };
        cloud::add_account(&app2.db, &p.label, &config)
    })
    .await;
    match r {
        Ok(Ok(id)) => {
            let _ = start_job(&app, "Listagem da nuvem", move |app, progress| sync_accounts(app, vec![id], progress));
            page("Conta conectada", "Pode fechar esta aba. A lista de arquivos da nuvem está sendo carregada.")
        }
        Ok(Err(e)) => page("Não foi possível conectar", &format!("{e:#}")),
        Err(e) => page("Não foi possível conectar", &e.to_string()),
    }
}

async fn cloud_sync(State(app): S) -> Api {
    start_job(&app, "Listagem da nuvem", |app, progress| {
        let ids = app.db.accounts()?.into_iter().map(|a| a.id).collect();
        sync_accounts(app, ids, progress)
    })
}

async fn cloud_remove(State(app): S, AxPath(id): AxPath<i64>) -> Api {
    blocking(&app, move |app| {
        app.db.remove_account(id)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

// ---- interface e segurança ---------------------------------------------------

async fn index() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}

async fn app_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../ui/app.js"))
}

async fn style_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../ui/style.css"))
}

/// O servidor só escuta em 127.0.0.1, mas uma página web maliciosa aberta no
/// mesmo navegador poderia tentar chamá-lo. Exigimos `Host` local (barra DNS
/// rebinding) e, se houver `Origin`, que seja a nossa própria interface.
async fn guard(State(app): S, headers: HeaderMap, req: Request, next: Next) -> Response {
    let allowed = [format!("localhost:{}", app.port), format!("127.0.0.1:{}", app.port)];
    let host_ok = headers.get(header::HOST).and_then(|h| h.to_str().ok()).is_some_and(|h| allowed.iter().any(|a| a == h));
    let origin_ok = match headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        None => true,
        Some(o) => allowed.iter().any(|a| o == format!("http://{a}")),
    };
    if !host_ok || !origin_ok {
        return (StatusCode::FORBIDDEN, "acesso permitido apenas pela interface local").into_response();
    }
    let mut res = next.run(req).await;
    res.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    res.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'"),
    );
    res
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/oauth/callback", get(oauth_callback))
        .route("/api/state", get(state))
        .route("/api/job", get(job))
        .route("/api/browse", get(browse))
        .route("/api/sources", post(add_source))
        .route("/api/sources/:id", delete(remove_source))
        .route("/api/scan", post(start_scan))
        .route("/api/adb/devices", get(adb_devices))
        .route("/api/adb/import", post(adb_import))
        .route("/api/duplicates", get(duplicates))
        .route("/api/short-videos", get(short_videos))
        .route("/api/blurry", get(blurry))
        .route("/api/optimize", get(optimize_list).post(optimize_run))
        .route("/api/enhance", post(enhance_run))
        .route("/api/library", get(library))
        .route("/api/backup", get(backup))
        .route("/api/trash", get(trash_list).post(trash_add))
        .route("/api/trash/restore", post(trash_restore))
        .route("/api/trash/empty", post(trash_empty))
        .route("/api/thumb/:sha1", get(thumb))
        .route("/api/file/:id", get(file))
        .route("/api/cloud/nextcloud", post(cloud_add_nextcloud))
        .route("/api/cloud/oauth/start", post(oauth_start))
        .route("/api/cloud/sync", post(cloud_sync))
        .route("/api/cloud/:id", delete(cloud_remove))
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data = cli
        .data_dir
        .or_else(|| dirs::data_local_dir().map(|d| d.join("PhotoMaster")))
        .context("não foi possível determinar a pasta de dados; use --data-dir")?;
    let dirs = Dirs::new(&data)?;
    let db = Db::open(&dirs.db_path())?;
    let app = Arc::new(App { db, dirs, port: cli.port, job: Mutex::new(Job::default()), oauth: Mutex::new(None) });

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cli.port))
        .await
        .with_context(|| format!("a porta {} está em uso; tente --port com outro número", cli.port))?;
    let url = format!("http://localhost:{}", cli.port);
    println!("PhotoMaster rodando em {url}  (Ctrl+C para sair)");
    println!("Dados em {}", data.display());
    if !media::ffmpeg_available() {
        println!("Aviso: ffmpeg não encontrado. Vídeos e fotos HEIC não serão analisados.");
    }
    if !cli.no_browser {
        let _ = open::that(&url);
    }
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
