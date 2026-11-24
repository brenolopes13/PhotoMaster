# PhotoMaster

Gerencie as fotos e os vídeos do celular a partir do computador: descubra o que
já tem backup na nuvem, o que está duplicado, o que pode ser descartado e o que
pode ocupar menos espaço.

> **Estado:** MVP para desktop (Windows e Linux). Os apps Android/iOS ainda não
> existem — veja [O que falta](#o-que-falta).

## O que já faz

| Recurso | Como funciona |
|---|---|
| **Backup na nuvem** | Lista os arquivos do Nextcloud, OneDrive e Google Drive (somente leitura) e compara com os seus por **hash de conteúdo** (SHA-1/MD5/SHA-256/QuickXorHash). Cada arquivo fica como *confirmado*, *provável* (mesmo nome e tamanho, sem hash disponível) ou *sem backup*. |
| **Duplicadas e parecidas** | Cópias idênticas e fotos quase iguais (hash perceptual dHash + aHash). Sugere manter a de maior resolução/nitidez. Sensibilidade ajustável. |
| **Vídeos curtos** | Vídeos de poucos segundos, com marcação extra para os *escuros* e *parados*. |
| **Fotos borradas** | Ordenadas por nitidez (variância do Laplaciano). |
| **Otimizar espaço** | Recomprime JPEGs "gordos" (preservando EXIF e perfil de cor) e converte vídeos para H.265 ou H.264. Só substitui se economizar ≥ 10%. |
| **Melhorar qualidade** | Fotos: contraste automático + nitidez. Vídeos: redução de ruído, nitidez e estabilização opcional. Sempre cria uma cópia; o original não muda. |
| **Lixeira** | Nada é apagado direto. Excluir e otimizar guardam o original na lixeira do app, de onde pode ser restaurado (inclusive de volta para o celular). |

## Como usar

Requisitos: [ffmpeg](https://ffmpeg.org/download.html) no `PATH` (vídeos e HEIC)
e, para Android via cabo, o [`adb`](https://developer.android.com/tools/releases/platform-tools).

```sh
cargo run --release -p photomaster
```

A interface abre em `http://localhost:7878`. Opções: `--port`, `--data-dir`, `--no-browser`.

### Conectar o celular

- **Android (USB):** ative a *Depuração USB*, conecte o cabo e use
  *Início → Conectar Android por USB*. As pastas `DCIM`, `Pictures`, `Movies` e
  a mídia do WhatsApp são espelhadas no computador para análise; excluir,
  otimizar e restaurar são replicados no aparelho.
- **iPhone no Linux:** com o aparelho montado (gvfs/`ifuse`), ele aparece em
  *Adicionar pasta…* como Dispositivo.
- **iPhone no Windows / qualquer outro caso:** copie a pasta de fotos para o
  computador e adicione a cópia com *Adicionar pasta…*.

### Conectar a nuvem

- **Nextcloud:** endereço do servidor, usuário e uma *senha de aplicativo*.
- **OneDrive / Google Drive:** o PhotoMaster não tem servidor nem credenciais
  embutidas; você registra um app gratuito na sua conta e informa o Client ID
  (a tela *Backup na nuvem* traz o passo a passo). Escopos pedidos:
  `Files.Read` (OneDrive) e `drive.metadata.readonly` (Google).

## Arquitetura

```
crates/core   biblioteca Rust: índice SQLite, hashes, análises, lixeira,
              otimização/melhoria (ffmpeg), ADB e clientes de nuvem
crates/app    binário desktop: servidor HTTP em 127.0.0.1 + UI web embutida
```

O núcleo não depende da interface, justamente para ser reaproveitado em apps
móveis. O servidor só escuta em `127.0.0.1` e recusa requisições com `Host` ou
`Origin` que não sejam da própria interface.

Testes (`cargo test`, precisa do ffmpeg) cobrem o fluxo de ponta a ponta com
mídia sintética, servidores falsos de WebDAV/Graph/Drive e um `adb` simulado.

## O que falta

- **Apps Android e iOS.** O plano é embrulhar o mesmo núcleo com Tauri 2 (ou
  expô-lo via FFI para UI nativa) para analisar a galeria no próprio aparelho.
- **iPhone no Windows** sem cópia manual (libimobiledevice ou app companheiro).
- **Testes com contas e aparelhos reais.** OneDrive, Google Drive e ADB foram
  validados apenas contra simuladores; o QuickXorHash foi implementado a partir
  da especificação e ainda precisa ser conferido com um arquivo real do OneDrive.
- **Enviar para a nuvem** o que está sem backup (hoje o app só confere).
- **Google Fotos:** a API não permite mais listar a biblioteca; só o Drive é suportado.
- Credenciais de nuvem ficam em texto puro no SQLite local; migrar para o
  cofre do sistema operacional.
- Melhoria por IA (super-resolução), conversão de PNG/HEIC, binários assinados
  e instaladores.

## Licença

GPL-3.0 — veja [LICENSE](LICENSE).
