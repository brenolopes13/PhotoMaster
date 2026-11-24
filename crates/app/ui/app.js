"use strict";
// PhotoMaster — interface. Todo conteúdo dinâmico entra via textContent
// (nunca innerHTML), então nomes de arquivo não conseguem injetar HTML.

const $ = (sel, el = document) => el.querySelector(sel);

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === false || v == null) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (k in el && k !== "list") el[k] = v;
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}

async function api(method, url, body) {
  const res = await fetch(url, {
    method,
    headers: body ? { "Content-Type": "application/json" } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `Erro ${res.status}`);
  return data;
}

const fmtBytes = (n) => {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toLocaleString("pt-BR", { maximumFractionDigits: i ? 1 : 0 })} ${u[i]}`;
};
const fmtDur = (s) => (s == null ? "" : s < 60 ? `${s.toFixed(1).replace(".", ",")} s` : `${Math.floor(s / 60)} min ${Math.round(s % 60)} s`);
const fmtDate = (secs) => new Date(secs * 1000).toLocaleDateString("pt-BR");
const plural = (n, one, many) => `${n.toLocaleString("pt-BR")} ${n === 1 ? one : many}`;

let toastTimer;
function toast(msg, isError) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = isError ? "error" : "";
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), isError ? 9000 : 5000);
}
const fail = (e) => toast(e.message || String(e), true);

function openDialog(...content) {
  const d = $("#dialog");
  d.replaceChildren(...content, h("div", { class: "row" }, h("span", { class: "spacer" }), h("button", { onclick: () => d.close() }, "Fechar")));
  d.showModal();
  return d;
}

function preview(m) {
  const src = `/api/file/${m.id}`;
  const view = m.kind === "video" ? h("video", { src, controls: true, autoplay: true }) : h("img", { src, alt: m.name });
  const d = openDialog(view, h("p", { class: "small muted" }, m.path));
  d.addEventListener("close", () => view.pause && view.pause(), { once: true });
}

// ---- seleção e cartões ------------------------------------------------------

let selected = new Map(); // id -> tamanho em bytes
let onSelectionChange = () => {};

function describe(m) {
  const parts = [fmtBytes(m.size)];
  if (m.width && m.height) parts.push(`${m.width}×${m.height}`);
  if (m.duration != null) parts.push(fmtDur(m.duration));
  return parts.join(" · ");
}

function card(m, opts = {}) {
  const id = opts.id ?? m.id;
  const box = h("input", {
    type: "checkbox",
    checked: selected.has(id),
    "aria-label": `Selecionar ${m.name}`,
    onchange: (e) => {
      e.target.checked ? selected.set(id, m.size) : selected.delete(id);
      onSelectionChange();
    },
  });
  const thumb = m.sha1 && !m.error
    ? h("img", { class: "thumb", loading: "lazy", alt: "", src: `/api/thumb/${m.sha1}`, onerror: (e) => e.target.replaceWith(h("div", { class: "thumb none" }, "sem miniatura")) })
    : h("div", { class: "thumb none" }, "sem miniatura");
  return h("label", { class: "card", title: opts.title || m.path || m.original_path },
    thumb, box,
    opts.noPreview ? null : h("button", { class: "view", type: "button", onclick: (e) => { e.preventDefault(); preview(m); } }, m.kind === "video" ? "▶ ver" : "ver"),
    h("div", { class: "info" },
      h("div", { class: "name" }, m.name),
      h("div", { class: "meta" }, describe(m)),
      opts.extra ? h("div", {}, opts.extra) : null));
}

function selectionBar(actions, allItems) {
  const count = h("span", { class: "muted" });
  const buttons = actions.map((a) => h("button", { class: a.class || "primary", onclick: () => a.run([...selected.keys()]) }, a.label));
  onSelectionChange = () => {
    const bytes = [...selected.values()].reduce((a, b) => a + b, 0);
    count.textContent = selected.size ? `${plural(selected.size, "selecionado", "selecionados")} · ${fmtBytes(bytes)}` : "Nada selecionado";
    buttons.forEach((b) => (b.disabled = !selected.size));
  };
  const bar = h("div", { class: "actionbar row" },
    allItems ? h("button", { onclick: () => { allItems().forEach(([id, size]) => selected.set(id, size)); render(false); } }, "Selecionar tudo") : null,
    h("button", { onclick: () => { selected.clear(); render(false); } }, "Limpar seleção"),
    count, h("span", { class: "spacer" }), buttons);
  onSelectionChange();
  return bar;
}

async function moveToTrash(ids) {
  const r = await api("POST", "/api/trash", { ids });
  const bad = r.results.filter((x) => !x.ok);
  selected.clear();
  toast(bad.length
    ? `${r.results.length - bad.length} movidos para a lixeira. ${bad.length} com problema:\n${bad.map((b) => `${b.name}: ${b.message}`).join("\n")}`
    : `${plural(r.results.length, "arquivo movido", "arquivos movidos")} para a lixeira. Dá para restaurar quando quiser.`, bad.length > 0);
  render();
}
const trashAction = { label: "Mover para a lixeira", class: "primary", run: (ids) => moveToTrash(ids).catch(fail) };

const header = (title, lead) => [h("h2", {}, title), h("p", { class: "lead" }, lead)];
const empty = (msg) => h("div", { class: "empty" }, msg);

// ---- telas --------------------------------------------------------------------

const settings = { dup: 6, shortMax: 3, blur: 40, backupFilter: "missing", libraryKind: "all", libraryLimit: 120 };
const views = {};

views.inicio = async () => {
  const st = await api("GET", "/api/state");
  const ov = st.overview;
  const out = [...header("Início", "Escolha onde estão suas fotos e vídeos e clique em Analisar. Nada é apagado sem você mandar, e tudo o que for excluído vai antes para a lixeira do PhotoMaster.")];
  out.push(h("div", { class: "stats" },
    h("div", { class: "stat" }, h("b", {}, ov.images.count.toLocaleString("pt-BR")), h("span", {}, `fotos · ${fmtBytes(ov.images.bytes)}`)),
    h("div", { class: "stat" }, h("b", {}, ov.videos.count.toLocaleString("pt-BR")), h("span", {}, `vídeos · ${fmtBytes(ov.videos.bytes)}`)),
    h("div", { class: "stat" }, h("b", {}, fmtBytes(ov.trash.bytes)), h("span", {}, `na lixeira (${ov.trash.count})`)),
    ov.errors ? h("div", { class: "stat warn" }, h("b", {}, ov.errors), h("span", {}, "arquivos que não foi possível ler")) : null));
  if (!st.ffmpeg) {
    out.push(h("div", { class: "panel" }, h("strong", {}, "ffmpeg não encontrado. "), "Sem ele, vídeos e fotos HEIC não são analisados nem otimizados. Instale o ffmpeg e reinicie o PhotoMaster."));
  }

  const list = st.sources.length
    ? st.sources.map((s) => h("div", { class: "list-item" },
        h("div", { class: "grow" }, h("strong", {}, s.label), " ", h("span", { class: "badge" }, s.kind === "adb" ? "Android (USB)" : "Pasta"), h("div", { class: "small muted" }, s.kind === "adb" ? `Aparelho ${s.device_serial}` : s.root)),
        s.kind === "adb" ? h("button", { onclick: () => api("POST", "/api/adb/import", { serial: s.device_serial, label: s.label }).then(pollJob).catch(fail) }, "Sincronizar") : null,
        h("button", { class: "danger", onclick: async () => { if (confirm(`Remover “${s.label}” da lista? Os arquivos não são apagados.`)) { await api("DELETE", `/api/sources/${s.id}`).catch(fail); render(); } } }, "Remover")))
    : [h("p", { class: "muted" }, "Nenhuma fonte ainda. Adicione uma pasta ou conecte um celular.")];
  out.push(h("h3", {}, "Onde estão as fotos"), h("div", { class: "panel" }, list,
    h("div", { class: "row", style: "margin-top:12px" },
      h("button", { onclick: () => browse() }, "Adicionar pasta…"),
      h("button", { onclick: () => androidDialog() }, "Conectar Android por USB…"),
      h("span", { class: "spacer" }),
      h("button", { class: "primary", disabled: !st.sources.length, onclick: () => api("POST", "/api/scan").then(pollJob).catch(fail) }, "Analisar agora"))));
  out.push(h("p", { class: "small muted" }, "iPhone: no Linux, o aparelho conectado aparece em “Adicionar pasta…” como Dispositivo. No Windows, copie antes a pasta DCIM pelo Explorador e adicione a cópia. ",
    "Dados do aplicativo: ", h("code", {}, st.data_dir)));
  return out;
};

async function browse(path) {
  let data;
  try { data = await api("GET", `/api/browse?path=${encodeURIComponent(path || "")}`); } catch (e) { return fail(e); }
  const add = async () => {
    try { await api("POST", "/api/sources", { path: data.path }); $("#dialog").close(); render(); } catch (e) { fail(e); }
  };
  openDialog(
    h("h3", { style: "margin-top:0" }, "Escolher pasta"),
    h("div", { class: "row" }, data.shortcuts.map((s) => h("button", { onclick: () => browse(s.path) }, s.name))),
    h("p", { class: "small" }, h("code", {}, data.path)),
    h("div", { class: "dirlist" },
      data.parent ? h("button", { onclick: () => browse(data.parent) }, "↑ Pasta acima") : null,
      data.dirs.map((d) => h("button", { onclick: () => browse(d.path) }, `📁 ${d.name}`)),
      data.dirs.length ? null : h("p", { class: "muted small", style: "padding:0 12px" }, "Sem subpastas.")),
    h("button", { class: "primary", onclick: add }, "Usar esta pasta"));
}

async function androidDialog() {
  let devices;
  try { devices = await api("GET", "/api/adb/devices"); } catch (e) { return fail(e); }
  const rows = devices.map((d) => h("div", { class: "list-item" },
    h("div", { class: "grow" }, h("strong", {}, d.model || d.serial), h("div", { class: "small muted" }, d.state === "device" ? `Pronto · ${d.serial}` : d.state === "unauthorized" ? "Desbloqueie o celular e toque em “Permitir depuração USB”." : `Estado: ${d.state}`)),
    h("button", { class: "primary", disabled: d.state !== "device", onclick: () => { $("#dialog").close(); api("POST", "/api/adb/import", { serial: d.serial, label: d.model }).then(pollJob).catch(fail); } }, "Importar e analisar")));
  openDialog(
    h("h3", { style: "margin-top:0" }, "Celular Android"),
    devices.length ? rows : h("p", {}, "Nenhum aparelho encontrado."),
    h("ol", { class: "steps" },
      h("li", {}, "No celular: Configurações → Sobre o telefone → toque 7 vezes em “Número da versão”."),
      h("li", {}, "Em Opções do desenvolvedor, ative “Depuração USB”."),
      h("li", {}, "Conecte o cabo e aceite o aviso que aparece no celular.")),
    h("p", { class: "small muted" }, "As fotos são copiadas para este computador para análise. O aparelho só é alterado quando você exclui, otimiza ou restaura algo."),
    h("button", { onclick: () => androidDialog() }, "Procurar de novo"));
}

views.duplicadas = async () => {
  const groups = await api("GET", `/api/duplicates?value=${settings.dup}`);
  const total = groups.reduce((a, g) => a + g.reclaimable, 0);
  const suggest = () => groups.flatMap((g) => g.items.slice(1).map((m) => [m.id, m.size]));
  const slider = h("input", { type: "range", min: 2, max: 12, value: settings.dup, onchange: (e) => { settings.dup = +e.target.value; selected.clear(); render(); } });
  return [
    ...header("Duplicadas e parecidas", "Cópias idênticas e fotos quase iguais (rajadas, reenvios, versões reduzidas). Em cada grupo, a primeira é a melhor versão; as demais são sugestões para excluir."),
    h("div", { class: "row small muted", style: "margin-bottom:8px" }, "Só quase idênticas", slider, "Mais abrangente", h("span", { class: "spacer" }), groups.length ? `${plural(groups.length, "grupo", "grupos")} · até ${fmtBytes(total)} para liberar` : ""),
    groups.length ? selectionBar([trashAction], null) : null,
    groups.length ? h("div", { class: "row", style: "margin-bottom:12px" }, h("button", { onclick: () => { suggest().forEach(([id, s]) => selected.set(id, s)); render(false); } }, "Selecionar todas as sugestões")) : null,
    groups.length ? groups.map((g) => h("section", { class: "group" },
      h("header", {}, h("span", { class: "badge" }, g.exact ? "Idênticas" : "Parecidas"), `${g.items.length} ${g.kind === "video" ? "vídeos" : "fotos"}`, h("span", { class: "muted" }, `libera ${fmtBytes(g.reclaimable)}`)),
      h("div", { class: "grid" }, g.items.map((m, i) => card(m, { extra: i === 0 ? h("span", { class: "badge ok" }, "Manter") : null })))))
      : empty("Nenhuma duplicada encontrada. Se ainda não analisou suas fotos, comece pelo Início."),
  ];
};

views.videos = async () => {
  const items = await api("GET", `/api/short-videos?value=${settings.shortMax}`);
  const input = h("input", { type: "number", min: 1, max: 60, value: settings.shortMax, style: "width:80px", onchange: (e) => { settings.shortMax = +e.target.value || 3; selected.clear(); render(); } });
  return [
    ...header("Vídeos curtos", "Gravações de poucos segundos costumam ser acidentais. As marcadas como “escuro” ou “parado” quase nunca têm conteúdo. Confira antes de excluir."),
    h("div", { class: "row small muted", style: "margin-bottom:8px" }, "Mostrar vídeos com até", input, "segundos"),
    items.length ? selectionBar([trashAction], () => items.map((m) => [m.id, m.size])) : null,
    items.length ? h("div", { class: "grid" }, items.map((m) => card(m, { extra: m.flags.map((f) => h("span", { class: "badge warn" }, f)) })))
      : empty("Nenhum vídeo curto encontrado."),
  ];
};

views.borradas = async () => {
  const items = await api("GET", `/api/blurry?value=${settings.blur}`);
  const slider = h("input", { type: "range", min: 10, max: 150, step: 5, value: settings.blur, onchange: (e) => { settings.blur = +e.target.value; selected.clear(); render(); } });
  return [
    ...header("Fotos borradas", "Fotos com pouca nitidez, da mais borrada para a menos. É uma estimativa: fotos de céu, neblina ou fundo desfocado de propósito também aparecem aqui."),
    h("div", { class: "row small muted", style: "margin-bottom:8px" }, "Só as muito borradas", slider, "Incluir levemente borradas"),
    items.length ? selectionBar([trashAction], null) : null,
    items.length ? h("div", { class: "grid" }, items.map((m) => card(m))) : empty("Nenhuma foto borrada encontrada."),
  ];
};

views.otimizar = async () => {
  const items = await api("GET", "/api/optimize");
  const total = items.reduce((a, m) => a + m.estimated_saving, 0);
  const codec = h("select", {}, h("option", { value: "hevc" }, "H.265/HEVC — menor (recomendado)"), h("option", { value: "h264" }, "H.264 — compatível com aparelhos antigos"));
  const run = (ids) => api("POST", "/api/optimize", { ids, options: { video_codec: codec.value } }).then(() => { selected.clear(); pollJob(); }).catch(fail);
  return [
    ...header("Otimizar espaço", "Arquivos gravados com muito mais dados do que precisam. A recompressão mantém resolução, data e metadados; o original fica na lixeira até você esvaziá-la."),
    items.length ? h("div", { class: "row small", style: "margin-bottom:8px" }, h("span", { class: "muted" }, "Vídeos em"), codec, h("span", { class: "spacer" }), h("strong", {}, `Economia estimada: ${fmtBytes(total)}`)) : null,
    items.length ? selectionBar([{ label: "Otimizar selecionados", run }], () => items.map((m) => [m.id, m.size])) : null,
    items.length ? h("div", { class: "grid" }, items.map((m) => card(m, { title: m.reason, extra: h("span", { class: "badge ok" }, `−${fmtBytes(m.estimated_saving)} estimado`) })))
      : empty("Nada para otimizar: seus arquivos já estão com tamanho razoável."),
  ];
};

views.melhorar = async () => {
  const all = await api("GET", "/api/library");
  const items = all.filter((m) => settings.libraryKind === "all" || m.kind === settings.libraryKind);
  const shown = items.slice(0, settings.libraryLimit);
  const kind = h("select", { onchange: (e) => { settings.libraryKind = e.target.value; render(); } },
    ...[["all", "Fotos e vídeos"], ["image", "Só fotos"], ["video", "Só vídeos"]].map(([v, t]) => h("option", { value: v, selected: settings.libraryKind === v }, t)));
  const stab = h("input", { type: "checkbox" });
  const run = (ids) => api("POST", "/api/enhance", { ids, options: { stabilize: stab.checked } }).then(() => { selected.clear(); pollJob(); }).catch(fail);
  return [
    ...header("Melhorar qualidade", "Fotos: contraste automático e nitidez. Vídeos: menos ruído, mais nitidez e, se quiser, estabilização. O original nunca é alterado: é criada uma cópia “_melhorada” ao lado."),
    h("div", { class: "row small", style: "margin-bottom:8px" }, kind, h("label", { class: "row" }, stab, "Estabilizar vídeos tremidos (mais lento)")),
    items.length ? selectionBar([{ label: "Melhorar selecionados", run }], null) : null,
    items.length ? h("div", { class: "grid" }, shown.map((m) => card(m))) : empty("Nenhum arquivo analisado ainda."),
    items.length > shown.length ? h("div", { class: "row", style: "margin-top:14px" }, h("button", { onclick: () => { settings.libraryLimit += 240; render(false); } }, `Mostrar mais (${items.length - shown.length} restantes)`)) : null,
  ];
};

views.backup = async () => {
  const [st, rep] = await Promise.all([api("GET", "/api/state"), api("GET", "/api/backup")]);
  const out = [...header("Backup na nuvem", "Compara cada arquivo com o que existe nas suas contas. “Confirmado” significa mesmo conteúdo, conferido por hash — não só o mesmo nome. O acesso às contas é somente leitura.")];

  const accounts = st.accounts.map((a) => h("div", { class: "list-item" },
    h("div", { class: "grow" }, h("strong", {}, a.label), " ", h("span", { class: "badge" }, { nextcloud: "Nextcloud", onedrive: "OneDrive", gdrive: "Google Drive" }[a.provider]),
      h("div", { class: "small muted" }, a.last_sync ? `${plural(a.files, "foto/vídeo", "fotos/vídeos")} na nuvem · lista de ${fmtDate(a.last_sync)}` : "Lista ainda não carregada")),
    h("button", { class: "danger", onclick: async () => { if (confirm(`Desconectar “${a.label}”? Nada é apagado na nuvem.`)) { await api("DELETE", `/api/cloud/${a.id}`).catch(fail); render(); } } }, "Desconectar")));
  out.push(h("div", { class: "panel" },
    accounts.length ? accounts : h("p", { class: "muted" }, "Nenhuma conta conectada."),
    accounts.length ? h("div", { class: "row", style: "margin-top:10px" }, h("button", { onclick: () => api("POST", "/api/cloud/sync").then(pollJob).catch(fail) }, "Atualizar lista da nuvem")) : null));

  const field = (label, input) => h("label", { class: "field" }, label, input);
  const ncUrl = h("input", { type: "text", placeholder: "https://nuvem.exemplo.com" });
  const ncUser = h("input", { type: "text", autocomplete: "username" });
  const ncPass = h("input", { type: "password", autocomplete: "off" });
  out.push(h("details", { class: "panel", open: !accounts.length },
    h("summary", {}, "Conectar Nextcloud"),
    h("p", { class: "small muted" }, "Use uma senha de aplicativo (no Nextcloud: Configurações → Segurança → Dispositivos e sessões), não a sua senha principal."),
    h("div", { class: "row" }, field("Endereço do servidor", ncUrl), field("Usuário", ncUser), field("Senha de aplicativo", ncPass)),
    h("div", { class: "row", style: "margin-top:12px" }, h("button", { class: "primary", onclick: () => api("POST", "/api/cloud/nextcloud", { url: ncUrl.value, user: ncUser.value, password: ncPass.value }).then(pollJob).catch(fail) }, "Conectar"))));

  const oauthForm = (provider, title, steps, needsSecret) => {
    const id = h("input", { type: "text" });
    const secret = h("input", { type: "password", autocomplete: "off" });
    const start = async () => {
      try {
        const r = await api("POST", "/api/cloud/oauth/start", { provider, client_id: id.value, client_secret: secret.value });
        window.open(r.url, "_blank", "noopener");
        toast("Conclua o login na aba que abriu. Depois volte aqui.");
        pollJob();
      } catch (e) { fail(e); }
    };
    return h("details", { class: "panel" }, h("summary", {}, title),
      h("p", { class: "small muted" }, "O PhotoMaster não tem servidor próprio, então você registra um aplicativo gratuito na sua conta (uma vez só):"),
      h("ol", { class: "steps" }, steps.map((s) => h("li", {}, s))),
      h("div", { class: "row" }, field("Client ID", id), needsSecret ? field("Client Secret", secret) : null),
      h("div", { class: "row", style: "margin-top:12px" }, h("button", { class: "primary", onclick: start }, "Entrar e autorizar")));
  };
  out.push(oauthForm("onedrive", "Conectar OneDrive", [
    "Acesse portal.azure.com → Microsoft Entra ID → Registros de aplicativo → Novo registro.",
    "Em tipos de conta, escolha “Contas em qualquer diretório organizacional e contas pessoais da Microsoft”.",
    ["Em URI de redirecionamento, escolha “Cliente público/nativo (móvel e desktop)” e informe ", h("code", {}, "http://localhost/oauth/callback"), "."],
    "Copie o “ID do aplicativo (cliente)” para o campo abaixo.",
  ], false));
  out.push(oauthForm("gdrive", "Conectar Google Drive", [
    "Acesse console.cloud.google.com, crie um projeto e ative a “Google Drive API”.",
    "Em “Tela de permissão OAuth”, configure como Externo e adicione seu próprio e-mail como usuário de teste.",
    "Em Credenciais → Criar credenciais → ID do cliente OAuth, escolha o tipo “App para computador”.",
    "Copie o Client ID e o Client Secret para os campos abaixo.",
  ], true));

  if (!st.accounts.length) return out;
  const tiles = [
    ["verified", "ok", "com backup confirmado", rep.verified],
    ["probable", "warn", "provavelmente com backup (mesmo nome e tamanho)", rep.probable],
    ["missing", "danger", "sem backup", rep.missing],
  ];
  out.push(h("h3", {}, "Situação dos seus arquivos"), h("div", { class: "stats" }, tiles.map(([key, cls, label, t]) =>
    h("button", { class: `stat ${cls} ${settings.backupFilter === key ? "active" : ""}`, onclick: () => { settings.backupFilter = key; selected.clear(); render(false); } },
      h("b", {}, t.count.toLocaleString("pt-BR")), h("span", {}, `${label} · ${fmtBytes(t.bytes)}`)))));
  const items = rep.items.filter((i) => i.status === settings.backupFilter);
  if (settings.backupFilter === "verified" && items.length) {
    out.push(h("p", { class: "small muted" }, "Estes arquivos têm cópia íntegra na nuvem. Se quiser liberar espaço, selecione e mova para a lixeira."),
      selectionBar([trashAction], () => items.map((m) => [m.id, m.size])));
  } else {
    onSelectionChange = () => {};
    if (settings.backupFilter === "missing" && items.length) out.push(h("p", { class: "small muted" }, "Envie estes arquivos pelo aplicativo da sua nuvem e depois clique em “Atualizar lista da nuvem” para confirmar."));
  }
  out.push(items.length ? h("div", { class: "grid" }, items.slice(0, 600).map((m) => card(m, { title: m.locations.join("\n") || m.path, extra: m.locations[0] ? h("div", { class: "meta name" }, m.locations[0]) : null })))
    : empty("Nenhum arquivo nesta situação."));
  if (items.length > 600) out.push(h("p", { class: "small muted" }, `Mostrando 600 de ${items.length}.`));
  return out;
};

views.lixeira = async () => {
  const items = await api("GET", "/api/trash");
  const total = items.reduce((a, t) => a + t.size, 0);
  const restore = async (ids) => {
    const r = await api("POST", "/api/trash/restore", { ids });
    const bad = r.results.filter((x) => !x.ok);
    selected.clear();
    toast(bad.length ? bad.map((b) => `${b.name}: ${b.message}`).join("\n") : `${plural(r.results.length, "arquivo restaurado", "arquivos restaurados")}.`, bad.length > 0);
    render();
  };
  const emptyTrash = async () => {
    if (!confirm(`Apagar definitivamente ${plural(items.length, "arquivo", "arquivos")} (${fmtBytes(total)})? Isto não pode ser desfeito.`)) return;
    const r = await api("POST", "/api/trash/empty").catch(fail);
    if (r) toast(`${fmtBytes(r.freed)} liberados.`);
    render();
  };
  return [
    ...header("Lixeira", "Tudo o que você excluiu ou otimizou pelo PhotoMaster fica guardado aqui, neste computador, até a lixeira ser esvaziada."),
    items.length ? selectionBar([{ label: "Restaurar selecionados", run: (ids) => restore(ids).catch(fail) }], () => items.map((t) => [t.id, t.size])) : null,
    items.length ? h("div", { class: "row", style: "margin-bottom:12px" }, h("span", { class: "muted" }, `${plural(items.length, "item", "itens")} · ${fmtBytes(total)}`), h("span", { class: "spacer" }), h("button", { class: "danger", onclick: emptyTrash }, "Esvaziar lixeira…")) : null,
    items.length ? h("div", { class: "grid" }, items.map((t) => card(t, { noPreview: true, extra: h("span", { class: "badge" }, t.reason === "otimizado" ? "original de otimização" : t.reason) })))
      : empty("A lixeira está vazia."),
  ];
};

// ---- navegação e tarefas -------------------------------------------------------

let current = "";
async function render(resetScroll = true) {
  const name = location.hash.slice(1) in views ? location.hash.slice(1) : "inicio";
  if (name !== current) { selected.clear(); current = name; }
  document.querySelectorAll("nav a").forEach((a) => a.classList.toggle("active", a.getAttribute("href") === `#${name}`));
  onSelectionChange = () => {};
  const y = window.scrollY;
  try {
    const content = await views[name]();
    $("#main").replaceChildren(...content.flat().filter(Boolean));
  } catch (e) {
    $("#main").replaceChildren(h("div", { class: "empty" }, `Não foi possível carregar: ${e.message}`));
  }
  window.scrollTo(0, resetScroll ? 0 : y);
  api("GET", "/api/state").then((st) => {
    const pill = $("#trash-count");
    pill.hidden = !st.overview.trash.count;
    pill.textContent = st.overview.trash.count;
  }).catch(() => {});
}

function summarize(job) {
  const r = job.result || {};
  const lines = [];
  for (const s of r.scans || []) lines.push(s.error ? `${s.source}: ${s.error}` : `${s.source}: ${plural(s.stats.found, "arquivo", "arquivos")}, ${s.stats.indexed} analisados agora${s.stats.errors ? `, ${s.stats.errors} ilegíveis` : ""}.`);
  for (const s of r.syncs || []) lines.push(s.error ? `${s.account}: ${s.error}` : `${s.account}: ${plural(s.files, "foto/vídeo encontrado", "fotos/vídeos encontrados")} na nuvem.`);
  if (r.results) {
    const ok = r.results.filter((x) => x.ok);
    const saved = ok.reduce((a, x) => a + Math.max(0, x.old_size - x.new_size), 0);
    lines.push(job.kind === "Otimização" ? `${plural(ok.length, "arquivo processado", "arquivos processados")}, ${fmtBytes(saved)} liberados.` : `${plural(ok.length, "arquivo melhorado", "arquivos melhorados")}.`);
    r.results.filter((x) => !x.ok).forEach((x) => lines.push(`${x.name}: ${x.message}`));
  }
  const hasError = (r.scans || []).some((s) => s.error) || (r.syncs || []).some((s) => s.error) || (r.results || []).some((x) => !x.ok);
  return [lines.join("\n") || `${job.kind}: concluído.`, hasError];
}

let polling = false;
let lastSeq = null;
async function pollJob() {
  if (polling) return;
  polling = true;
  try {
    for (;;) {
      const job = await api("GET", "/api/job");
      if (lastSeq === null) lastSeq = job.running ? job.seq - 1 : job.seq;
      $("#jobbar").hidden = !job.running;
      if (job.running) {
        $("#job-kind").textContent = job.kind;
        $("#job-msg").textContent = job.total ? `${job.done}/${job.total} · ${job.message}` : job.message;
        const p = $("#job-progress");
        if (job.total) { p.max = job.total; p.value = job.done; } else p.removeAttribute("value");
      } else {
        if (job.seq !== lastSeq) {
          lastSeq = job.seq;
          if (job.error) toast(`${job.kind}: ${job.error}`, true);
          else toast(...summarize(job));
          render(false);
        }
        break;
      }
      await new Promise((r) => setTimeout(r, 600));
    }
  } catch (e) { /* servidor fechado */ } finally { polling = false; }
}

window.addEventListener("hashchange", () => render());
window.addEventListener("focus", pollJob);
render();
pollJob();
