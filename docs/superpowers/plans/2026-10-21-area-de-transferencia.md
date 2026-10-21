# lucernad: o módulo da área de transferência — plano de implementação

> **Para agentes:** SUB-SKILL OBRIGATÓRIO: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para implementar este plano tarefa por tarefa. Os passos usam checkbox (`- [ ]`) para acompanhamento.

**Objetivo:** o módulo `clipboard` do `lucernad` (observação pelo protocolo de controle de dados do Wayland, histórico em SQLite com busca trigram, imagens com miniatura, cópia de volta segurada pelo daemon) e o painel `Super+V` do shell lendo dele, com o `wl-clipboard` fora.

**Arquitetura:** dois repositórios. `lucernad`: o crate novo `lucernad-clipboard` (o Wayland atrás do trait `Backend`, a escolha do tipo e a leitura do pipe como funções puras, a busca e a miniatura puras, o banco no `Store`, e o laço guiado pelo assinante, pelo `configure` e pela seleção nossa) e a feature `clipboard` (padrão) no binário, com os subcomandos `historico` e `copiar`. `lucerna-shell`: o `Clipboard.qml` vira adaptador (assina, configura, pagina), o `ClipboardState` e o painel passam a mostrar páginas do daemon com o filtro "Arquivos" e o trecho marcado, e a captura de tela copia pelo daemon. O `lucerna-sdk` não muda.

**Tecnologia:** Rust 1.95 (edição 2024); `wayland-client` 0.31 (0.31.15, backend Rust puro, sem `libwayland`), `wayland-protocols` 0.32 (0.32.13, `client` + `staging`: `ext::data_control::v1`), `wayland-protocols-wlr` 0.3 (0.3.12, `client`: `data_control::v1`), `rusqlite` 0.40 (0.40.2, `bundled`: SQLite 3.53.2), `image` 0.25 (0.25.10, só `png`, `jpeg`, `webp`), `sha2` 0.10 (0.10.9), `unicode-normalization` 0.1, `rustix` 1 (já no workspace), `lucerna-paths` do SDK; QML do Quickshell 0.3.2; sway 1.12 headless (sonda e fumaça), Hyprland 0.56.2 (container do `dev/test.sh`), KWin 6.8 (a máquina do usuário); GitHub Actions; `gh`.

**Spec:** `docs/superpowers/specs/2026-10-21-area-de-transferencia-design.md` (neste repositório, na `main` desde o a5f4a6e, PR #10; leia inteiro antes da primeira tarefa). Base: os specs e planos do sistema e do espectro (`docs/superpowers/plans/2026-10-20-espectro.md`), cujos padrões este plano segue.

## Global Constraints

- **Ordem de execução:** Tarefas 1–6 (`lucernad`, branch `feat/clipboard`) → Tarefas 8–9 (`lucerna-shell`, branch `feat/area-de-transferencia`; a 8 não depende do `lucernad` e pode andar em paralelo com 1–6; a conferência visual da 9 precisa do binário da 6) → Tarefa 7 (conferência na máquina real, que precisa do shell da 9; PR e merge do `lucernad`) → Tarefa 10 (precisa do `<LUCERNAD_REV>`; PR e merge do shell). Os números seguem o repositório, não a ordem.
- **Branches:** no `lucernad`, a `feat/clipboard` parte de `origin/main` (a5f4a6e), e o primeiro commit dela é este plano (feito pelo controlador). No `lucerna-shell`, `git switch -c feat/area-de-transferencia origin/main` no checkout principal (o `dev/test.sh` monta esse checkout).
- **Merge:** cada PR é aberto pela tarefa que fecha o repositório; o merge é do **controlador**, por merge commit (`gh pr merge <n> --merge`), depois da revisão final da branch (Opus) e do CI verde. Os rulesets continuam exigindo `fmt`/`clippy`/`test` (`lucernad`) e `lint`/`rules`/`unit`/`smoke` (shell).
- Contas e remotes: `gh` com a conta `EduardoSA8006`; o `origin` é `github-pessoal:lucerna-desktop/<repo>.git`.
- Toolchain: `cargo`/`rustc` em `~/develop/cargo/bin` (ponha no `PATH`); `rust-toolchain.toml` com `channel = "1.95"`. O SDK continua em `rev = "f55ba66dc18b62fab075b20691ad9edb7ca22a1c"`. No zsh, `git show "${R}:crates/..."` (o `:c` sem chaves vira modificador).
- Dependências novas só em `[workspace.dependencies]`, exatamente: `wayland-client = "0.31"`, `wayland-protocols = { version = "0.32", features = ["client", "staging"] }`, `wayland-protocols-wlr = { version = "0.3", features = ["client"] }`, `rusqlite = { version = "0.40", features = ["bundled"] }`, `image = { version = "0.25", default-features = false, features = ["png", "jpeg", "webp"] }`, `sha2 = "0.10"`, `unicode-normalization = "0.1"`, `lucerna-paths = { git = …, rev = <a mesma> }`, `lucernad-clipboard = { path = "crates/lucernad-clipboard" }`; o `rustix` existente ganha a feature `event` (o `poll`). `unsafe_code = "deny"` vale para tudo (a sonda compilou o Wayland, o pipe pelo `std::io::pipe` e o `poll` sem `unsafe`).
- **Sem dependência de sistema nova:** o `wayland-client` fica no backend Rust (sem a feature `client_system`), o `rusqlite` compila o SQLite com o `gcc` que o CI e o container já têm, e o `image` é Rust puro. O CI não ganha `apt-get`.
- **Nenhum processo externo** em `crates/*/src`: nada de `std::process::Command` nem `tokio::process` (os testes e a sonda podem usar `wl-copy`/`wl-paste` **dentro do container isolado**).
- **Nunca tocar na área de transferência nem nos dados do usuário** fora da Tarefa 7: sonda e testes só em compositor isolado (o `tests/isolado.sh` da Tarefa 4, a fumaça do `dev/ci.sh`, o container do `dev/test.sh`); todo teste com `XDG_DATA_HOME` e `XDG_RUNTIME_DIR` temporários. Na Tarefa 7, quem copia é o usuário, e o daemon de teste usa diretórios temporários.
- **No container do `dev/test.sh`, só o Hyprland aninhado.** O `dev/test.sh` e o `dev/run.sh` montam o socket do KWin do usuário como `wayland-host` e põem `WAYLAND_DISPLAY=wayland-host` no ambiente do container; como o `WAYLAND_DISPLAY` do ambiente vence o do `configure`, um `lucernad` ou `lucernad-selecao` com esse ambiente mexeria na área de transferência real. Todo `lucernad` e `lucernad-selecao` do container sobe e roda **só** pelo `dev/test.sh run` (o `ex` troca o `WAYLAND_DISPLAY` pelo `wayland-[0-9]*` do Hyprland aninhado), nunca por `docker exec` direto; e, antes de qualquer cópia, `dev/test.sh run 'tr "\0" "\n" < /proc/$(pgrep -xo lucernad)/environ | grep ^WAYLAND_DISPLAY='` mostra `wayland-` seguido de número (nunca `wayland-host`); se mostrar outro, pare.
- Tópicos, métodos, textos de erro e limites exatamente como em "Formato" (abaixo).
- Textos em português do Brasil, com acentos (mensagens, comentários, documentação, commits no estilo `Área de transferência: a escolha do tipo e a leitura do pipe`); nomes de código em inglês.
- Regras do shell: `qmllint` com 0 avisos; 100% das funções do escopo de `tests/coverage.json` chamadas pelos testes; camadas do `ci/rules.py` (`features/*/ui` não importa `qs.services`); funções declaradas como `function nome(...) {` numa linha; um teste que muda um singleton restaura o estado no fim; **todo dado que chega do daemon passa por `Array.from` antes de `Array.isArray`/`map`/`filter`** (o sinal `received` e o `modelData` entregam lista do QML, não `Array`: o bug do PR #8).
- Verificação de toda tarefa Rust, além do teste dela: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo clippy -p lucernad --no-default-features --all-targets --locked -- -D warnings`, o mesmo com `--features system`, `--features spectrum` e (da Tarefa 6 em diante) `--features clipboard`, e `TMPDIR=/tmp cargo test --workspace --locked` limpos. O `Cargo.lock` atualizado vai no commit da tarefa que põe a dependência. Toda tarefa do shell: `dev/ci.sh lint rules unit` (e `smoke` na Tarefa 10) verdes.

## Review Focus

1. **Texto com emoji ou acento antes do trecho buscado** (`"😀 Ação"` buscando `acao`): o intervalo marcado cai exatamente em "Ação" no QML, que conta em UTF-16. Testes: `matches_are_utf16_ranges` (Tarefa 2) e `clipboardstate: o trecho marcado pelo modelData` (Tarefa 9).
2. **App que não fecha o pipe, manda além do limite ou uma imagem-bomba** (100 000 × 100 000 px num PNG pequeno): a cópia é descartada com log, a próxima entra, nada trava. Testes: `stuck_writer_times_out` e `over_limit_is_refused` (Tarefa 1), `huge_dimensions_are_refused` (Tarefa 2), `stuck_offer_does_not_block_the_next_copy` (Tarefa 5).
3. **O shell recarrega logo depois de colar** (o assinante sai com a seleção nossa): a conexão e o `WorkGuard` ficam até outro app copiar; aí a conexão é solta. Testes: `owned_selection_outlives_the_subscriber` (Tarefa 5) e o passo 5 da conferência (Tarefa 7).
4. **A mesma imagem copiada de novo, ou duas cópias seguidas rápidas** (a segunda chega antes de a primeira terminar de ler): um item só, um arquivo só, e a ordem do histórico é a das cópias. Testes: `same_image_twice_is_one_item_and_one_file` (Tarefa 3) e `fast_copies_keep_their_order` (Tarefa 5).
5. **Desligar e religar a persistência com imagens no histórico** (o tmpfs e o disco são sistemas de arquivos diferentes: `rename` falha com `EXDEV`): os itens e os arquivos passam de um lado ao outro, nada fica órfão nem no disco depois de desligar. Teste: `persist_off_then_on_moves_items_and_files` (Tarefa 3).

---

## Decisões deste plano (o que o spec deixava ao construtor)

| Tema | Decisão | Por quê |
| --- | --- | --- |
| Crate | `crates/lucernad-clipboard` (biblioteca, feature `clipboard` padrão no binário), com `testing` como os outros | o padrão da casa |
| Wayland | `wayland-client` sem `libwayland`; prefere `ext_data_control_manager_v1` e cai no `zwlr_data_control_manager_v1` (v2); o primeiro `wl_seat`; uma `std::thread` por sessão com `rustix::event::poll` sobre o fd da conexão e um pipe de despertar (os comandos chegam por `std::sync::mpsc` + um byte no pipe); eventos ao laço por `tokio::sync::mpsc` | o KWin do usuário só tem o `ext` (v1); o sway 1.12 e o Hyprland 0.56 têm os dois; sem `AsyncFd` (o `AsyncFd::new` do tokio 1.54 está obsoleto) |
| Exibição | o `WAYLAND_DISPLAY` do ambiente do daemon vence; sem ele, o do `configure`; nome relativo vai contra o `XDG_RUNTIME_DIR`, absoluto vale como está; o shell manda sempre o dele | o shell não sabe o ambiente do daemon; o caminho absoluto deixa a Tarefa 7 usar um `XDG_RUNTIME_DIR` próprio |
| Cópias nossas | a fonte do `lucernad` oferece também `application/x-lucernad-item` (o id do item, em texto); uma seleção com esse tipo só sobe o item ao topo, sem ler | determinístico, sem depender da ordem entre `cancelled` e `selection` |
| Sensível | basta o tipo `x-kde-passwordManagerHint` ou `application/x-kde-passwordManagerHint` estar na lista; o valor é ignorado (nem lido) | o spec manda não ler nada; é o que o script de hoje faz. **Para o PR:** mais rígido que o spec, que fala no valor `secret` |
| Leitura | um pedido por cópia, lido fora do laço (tarefa com `poll` até o prazo); o resultado volta ao laço com o número de ordem da cópia; prazo de 3 s; texto e arquivos até 1 MiB, imagem até 32 MiB; texto vazio ou só com espaços é ignorado; texto fora do UTF-8 vira `from_utf8_lossy` | nada trava o laço (Review Focus 2) |
| Imagens | `png`, `jpeg` e `webp` ganham miniatura e dimensões; outras `image/*` entram sem miniatura (o painel mostra o ícone); decodificação com `image::Limits` (lado até 16 384 px, alocação até 512 MiB) | a bomba de pixels não derruba o daemon |
| Hash | SHA-256 de `kind`, um byte zero e o conteúdo, em hex | o mesmo texto como texto e como lista de arquivos não colide |
| Esquema | `items(id INTEGER PRIMARY KEY, kind TEXT, hash TEXT UNIQUE, text TEXT, norm TEXT, mime TEXT, width INT, height INT, size INT, lines INT, file TEXT, created INT, used INT, pinned INT)`; índice em `(pinned DESC, used DESC)`; `items_fts` = `fts5(norm, content='items', content_rowid='id', tokenize='trigram')` com gatilhos de inclusão, remoção e troca; `PRAGMA user_version = 1`; WAL e `synchronous = NORMAL` no disco | o FTS indexa o texto já normalizado em Rust, então a busca curta e a longa dizem "casa" do mesmo jeito |
| Normalização | NFD (`unicode-normalization`), tira as marcas combinantes (`Mn`), `to_lowercase` | a mesma regra para o FTS, a comparação curta e o trecho marcado |
| Busca | consulta normalizada com 3 caracteres ou mais: `items_fts MATCH '"<consulta com " dobradas>"'`; com 1 ou 2: `instr(norm, ?) > 0`; imagens nunca casam; para arquivos, o `norm` são os nomes dos arquivos (decodificados do `%XX`) | a frase entre aspas tira a sintaxe do FTS5 da consulta do usuário |
| Prévia e trecho | `preview` com até 400 caracteres, espaços colapsados; com consulta e o primeiro casamento depois do caractere 300, a prévia começa 40 caracteres antes, com "…"; `matches` = até 20 pares `[início, fim)` em **unidades UTF-16** da `preview` | o QML corta em UTF-16 (Review Focus 1); o shell nunca recebe o texto grande |
| Página | `clipboard.list` aceita `limit` de 1 a 200; o painel pede 50 e mais 50 ao chegar no fim da lista; ao ver `revision` nova, repede de 0 até o tanto que já mostrava | o painel não carrega milhares de itens |
| Campos além do spec | `lines` nos itens de texto (o detalhe "N caracteres · N linhas" de hoje; `size` de texto = caracteres, de imagem e arquivos = bytes) e `pinned` no `clipboard.history` (a página de configurações mostra "N fixados") | o painel e as configurações iguais aos de hoje, sem baixar o histórico. **Para o PR.** |
| Método além do spec | `clipboard.copyImage {path, store?}` → `{id}`: lê uma imagem do disco (até 32 MiB), com o mime pela assinatura dos primeiros bytes (`image::guess_format`, não a extensão), guarda como item se `store` (padrão `true`; o shell passa `Config.clipboardEnabled`) e vira dono da seleção; sem guardar, `id` é `0` e a fonte não tem `OWN_MIME` | a captura de tela copiava pelo `wl-copy` (`Capture.qml:121`, o único `wl-copy` fora do `Clipboard.qml`), que sai; com o histórico desligado, a captura não pode entrar nele. **Para o PR.** |
| Tempo | `created` e `used` em milissegundos desde a época (o `Format.since` do shell) | o mesmo de hoje |
| Configuração | os métodos (salvo o `configure`) e o primeiro assinante esperam a primeira configuração por até 2 s; depois, o padrão (`limit` 100, `persist` ligado) | o "só observa depois do primeiro configure (ou 2 s)" do spec, estendido aos métodos para a linha de comando. **Para o PR.** |
| Seleção segurada | enquanto a seleção é nossa, o módulo guarda um `WorkGuard` do `ServiceBuilder::work()` (o `register` recebe o `WorkTracker`) e a sessão fica aberta sem assinante; o `cancelled` solta os dois | o `WorkTracker` só sai do `ServiceBuilder` |
| Servir a seleção | cada `send` vira uma thread que escreve tudo e fecha o fd (erro de pipe quebrado ignorado); o conteúdo fica num `Arc<[u8]>` | quem cola devagar não segura o laço do Wayland |
| Itens desconhecidos | `remove`/`pin` de id que não existe → `{}`; `copy`/`get` → erro `1001` "O item <id> não existe mais." | apagar duas vezes não é erro; copiar o que sumiu é |
| Linha de comando | `lucernad historico [BUSCA]` (`clipboard.list` com 20 itens, uma linha `id  tipo  prévia` cada) e `lucernad copiar ID` | a conferência e a fumaça sem o shell |
| Copiar sem `wl-clipboard` na fumaça | o exemplo `crates/lucernad-clipboard/examples/selecao.rs` (`selecao oferecer MIME… < dados`, `selecao ler MIME`, `selecao tipos`), feito sobre o próprio backend Wayland; o `dev/lucernad.sh` o instala como `lucernad-selecao`, ao lado do `lucernad` | o `Quickshell.clipboardText` só faz texto e não marca o tipo sensível; a sonda usa o `wl-copy` de verdade, só dentro do container isolado, para o backend não se testar contra ele mesmo |
| Trabalho pesado fora do lock | o `Store` fica num `Mutex` que só prende a transação: o hash, a normalização, a miniatura e a gravação dos arquivos (`Store::prepare`, função livre) rodam antes, em `spawn_blocking`, sem o lock; o `content(id)` do `copy` também em `spawn_blocking`; a leitura do pipe (`read_pipe`, bloqueante) em `spawn_blocking`, nunca numa thread do runtime | uma imagem de 32 MiB não trava o `clipboard.list` |
| Memória sem persistência | o `Store` em memória apaga a pasta dos arquivos no `Drop` (não num `close` explícito): o `ModuleTask` largado aborta o laço sem passar por código de saída; `Store::open(Memory)` apaga antes o `history.db*` e o `images/` do disco (o primeiro `configure` já com `persist` desligado) | "sem persistência" vale também para a queda e para o histórico de antes |
| Prazos | conexão 2 s; leitura 3 s; espera do `configure` 2 s; nova tentativa a cada 5 s com assinante | nada trava o laço |

## Formato

```jsonc
// clipboard.history — tópico comum; null até o banco abrir
{"revision":12,"count":87,"pinned":3}

// clipboard.status — tópico comum
{"available":true,"error":null}

// clipboard.list {kind?: "text"|"image"|"files", query?: string, offset: int ≥ 0, limit: 1–200}
{"items":[
  {"id":41,"kind":"text","preview":"…fica a Ação do dia","size":5120,"lines":80,"created":1761060000000,"used":1761060100000,"pinned":false,"matches":[[13,17]]},
  {"id":40,"kind":"image","image":"/…/images/<hash>.png","thumb":"/…/images/<hash>.thumb.png","mime":"image/png","width":1920,"height":1080,"size":482113,"created":…,"used":…,"pinned":true},
  {"id":39,"kind":"files","preview":"relatório.pdf, foto.jpg","uris":["file:///home/…/relat%C3%B3rio.pdf","file:///home/…/foto.jpg"],"size":64,"created":…,"used":…,"pinned":false}
],"total":3}
```

- `clipboard.get {id}` → o item com `text` inteiro (texto) ou as `uris` (arquivos); `clipboard.copy {id}`, `clipboard.remove {id}`, `clipboard.pin {id, pinned}`, `clipboard.clear {all?}` → `{}`; `clipboard.copyImage {path, store?}` → `{id}`; `clipboard.configure {limit, persist, waylandDisplay?}` → `{}`.
- Erros `-32602` (`RpcError::invalid_params`, texto **sem** ponto final; o SDK põe "Parâmetros inválidos: " e o ponto): `"id precisa ser o número de um item"`, `"pinned precisa ser verdadeiro ou falso"`, `"all precisa ser verdadeiro ou falso"`, `"kind precisa ser text, image ou files"`, `"offset precisa ser um inteiro a partir de 0"`, `"limit precisa ser um inteiro entre 1 e 200"` (no `list`), `"query precisa ser um texto de até 1000 caracteres"`, `"limit precisa ser um inteiro entre 1 e 1000"` (no `configure`), `"persist precisa ser verdadeiro ou falso"`, `"waylandDisplay precisa ser um texto"`, `"path precisa ser um caminho absoluto"`, `"store precisa ser verdadeiro ou falso"`.
- Erros do app: `1001` "O item <id> não existe mais."; `1002` "Não consegui ler a imagem: <e>." (também quando os primeiros bytes não são de imagem: "<path> não é uma imagem"); `1003` "A área de transferência está indisponível: <error do status>." (o `copy` sem sessão possível).
- `clipboard.status.error`, os textos: "Sem WAYLAND_DISPLAY: o lucernad não sabe qual sessão observar.", "Não consegui conectar ao Wayland (<caminho>): <e>.", "O compositor não respondeu em 2 s.", "O compositor não oferece o controle da área de transferência (ext-data-control-v1 nem wlr-data-control).", "O compositor fechou a conexão: <e>.", "Não consegui abrir o histórico: <e>.", "Sem XDG_DATA_HOME nem HOME: não sei onde guardar o histórico.", "O módulo da área de transferência parou por um erro interno.".
- Caminhos: disco em `$XDG_DATA_HOME/lucerna/clipboard/{history.db,images/}` (`AppPaths::new("clipboard", &env)?.data_dir()`); sem persistência, os arquivos em `$XDG_RUNTIME_DIR/lucerna/clipboard/images/` (`AppPaths::runtime_dir()` + `clipboard`), apagados ao abrir e ao parar.

## Mapa de arquivos

**`lucernad`:**

```
Cargo.toml  Cargo.lock  README.md  .github/workflows/ci.yml
crates/lucernad-clipboard/
  Cargo.toml
  src/lib.rs        ClipboardConfig, ClipboardTopics, register, Clipboard, o laço, os métodos
  src/mime.rs       Kind, Choice, choose, offers_for, constantes de tipo e de limite
  src/pipe.rs       read_pipe, ReadError, socket_path
  src/search.rs     normalize, find_matches, preview_for, utf16_ranges
  src/thumb.rs      Thumb, thumbnail, extension_for
  src/store.rs      Store, NewClip, Item, ListQuery, Page
  src/backend.rs    Event, Source, Control, Session, Backend
  src/wayland.rs    Wayland, Protocol (o backend de verdade)
  src/testing.rs    (feature "testing") FakeBackend, png_bytes, jpeg_bytes, fake_config, quiet_config
  examples/selecao.rs
  tests/{mime,pipe,search,thumb,store,backend,module,sonda}.rs  tests/isolado.sh
crates/lucernad/  Cargo.toml  src/{lib,main}.rs  tests/{common/mod.rs,protocol.rs,process.rs}
```

**`lucerna-shell`:** `services/Clipboard.qml` (reescrito), `services/Capture.qml`, `services/scripts/clipboard-watch.sh` (sai), `features/clipboard/state/ClipboardState.qml`, `features/clipboard/ui/{Clipboard,ClipboardItem}.qml`, `features/settings/{state/ClipboardSettings,ui/ClipboardPage}.qml`, `tests/clipboard.test.js` (reescrito), `tests/clipboardstate.test.js` (novo), `tests/runner.qml`, `tests/coverage.json`, `dev/{lucernad.rev,lucernad.sh,Dockerfile,test.sh}`, `ci/{smoke.sh,tolerated.txt}`, `README.md`, `Lucerna — Pendências.md`.

---

### Tarefa 1: `lucernad` — a escolha do tipo, o sensível e a leitura do pipe

Repositório `lucernad`, branch `feat/clipboard` a partir de `origin/main`, já com o commit deste plano.

**Files:**
- Modify: `Cargo.toml` (o membro entra pelo `crates/*`; `lucernad-clipboard`, `rustix` com `event`)
- Create: `crates/lucernad-clipboard/Cargo.toml` (`description = "Módulo clipboard do lucernad: o histórico da área de transferência do Wayland."`, `[features] testing = []`, ele mesmo com `testing` em `[dev-dependencies]`, `tempfile`), `src/lib.rs` (só os `pub mod` desta tarefa e a doc do crate), `src/mime.rs`, `src/pipe.rs`
- Test: `crates/lucernad-clipboard/tests/{mime,pipe}.rs`

**Interfaces:**
- Consumes: nada das outras tarefas.
- Produces:
  - `mime`: `pub const TEXT_MAX: usize = 1 << 20`, `IMAGE_MAX: usize = 32 << 20`, `READ_TIMEOUT: Duration` (3 s), `OWN_MIME: &str = "application/x-lucernad-item"`, `SECRET_MIMES: [&str; 2]`, `TEXT_MIMES: [&str; 5]` (`text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`, `TEXT`, `STRING`, nesta ordem), `URI_LIST: &str`, `GNOME_FILES: &str = "x-special/gnome-copied-files"`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Kind { Text, Image, Files }` com `as_str()` (`"text"`, `"image"`, `"files"`), `parse(&str) -> Option<Kind>` e `max_size()`.
  - `#[derive(Debug, PartialEq, Eq)] pub enum Choice { Secret, Own, Read { kind: Kind, mime: String }, Nothing }`; `pub fn choose(mimes: &[String]) -> Choice` (nesta ordem: sensível, nosso, `image/png`, outra `image/*` na ordem oferecida, `text/uri-list`, os `TEXT_MIMES` na ordem acima; HTML e o resto → `Nothing`).
  - `pub fn offers_for(kind: Kind, mime: Option<&str>, data: &[u8], id: i64) -> Vec<(String, Arc<[u8]>)>` (texto: os cinco `TEXT_MIMES` com os mesmos bytes; imagem: o `mime` do item; arquivos: `text/uri-list` e `x-special/gnome-copied-files` = `"copy\n"` + as URIs separadas por `\n`, sem os comentários `#`; sempre por último `OWN_MIME` com o `id` em texto).
  - `pipe`: `#[derive(Debug, PartialEq, Eq)] pub enum ReadError { TooBig, TimedOut, Io(String) }`; `pub fn read_pipe(reader: std::io::PipeReader, max: usize, deadline: Instant) -> Result<Vec<u8>, ReadError>` (`rustix::event::poll` com o tempo que falta, `read` quando pronto; passou de `max` → `TooBig` na hora, sem ler o resto); `pub fn socket_path(display: &str, runtime_dir: Option<&Path>) -> Result<PathBuf, String>` (absoluto como está; relativo junto ao `runtime_dir`; sem `runtime_dir` → `Err("Sem XDG_RUNTIME_DIR para achar o Wayland.")`; vazio → `Err` do `WAYLAND_DISPLAY` do Formato).

- [ ] **Passo 1: os testes.**
  - `tests/mime.rs` (`m(&[…])` monta o `Vec<String>`):
    - `secret_wins_over_everything`: `["text/plain", "x-kde-passwordManagerHint"]` e `["image/png", "application/x-kde-passwordManagerHint"]` → `Secret`.
    - `own_copy_is_recognized`: `["text/plain", OWN_MIME]` → `Own`.
    - `image_before_files_before_text`: `["text/plain", "image/jpeg", "image/png"]` → `Read { Image, "image/png" }`; `["text/plain", "image/webp"]` → `Read { Image, "image/webp" }`; `["text/plain;charset=utf-8", "text/uri-list"]` → `Read { Files, "text/uri-list" }`; `["STRING", "text/plain", "UTF8_STRING"]` → `Read { Text, "text/plain" }`.
    - `html_alone_is_nothing`: `["text/html"]` e `[]` → `Nothing`.
    - `offers_cover_each_kind`: texto → os 5 tipos de texto + `OWN_MIME` (`"7"`), todos os de texto com os mesmos bytes; arquivos de `"file:///a\r\n# c\r\nfile:///b\r\n"` → `GNOME_FILES` = `"copy\nfile:///a\nfile:///b"`; imagem `image/jpeg` → `["image/jpeg", OWN_MIME]`.
    - `kind_round_trip`: `Kind::parse("files")` = `Some(Files)`, `parse("x")` = `None`, `Files.max_size()` = `TEXT_MAX`, `Image.max_size()` = `IMAGE_MAX`.
  - `tests/pipe.rs` (pipes do `std::io::pipe()`, quem escreve numa thread):
    - `reads_until_eof`: 300 000 bytes escritos e fechados → `Ok` com os mesmos bytes.
    - `over_limit_is_refused`: 1 MiB + 1 byte com `max` = `TEXT_MAX` → `Err(TooBig)`, em menos de 1 s.
    - `stuck_writer_times_out`: quem escreve manda 10 bytes e não fecha → `Err(TimedOut)` com prazo de 200 ms, voltando entre 150 e 500 ms.
    - `socket_paths`: `("wayland-1", Some("/run/user/1000"))` → `/run/user/1000/wayland-1`; `("/tmp/x/wayland-9", None)` → o mesmo; `("wayland-1", None)` → `Err` com "XDG_RUNTIME_DIR".
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-clipboard` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-clipboard --test mime --test pipe` → 10 passed; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: a escolha do tipo, o sensível e a leitura do pipe"`

---

### Tarefa 2: `lucernad` — a busca e a miniatura

**Files:**
- Create: `crates/lucernad-clipboard/src/{search,thumb}.rs`; em `src/testing.rs`, `png_bytes` e `jpeg_bytes`
- Modify: `Cargo.toml` (`unicode-normalization`, `image`), `crates/lucernad-clipboard/Cargo.toml`
- Test: `crates/lucernad-clipboard/tests/{search,thumb}.rs`

**Interfaces:**
- Consumes: nada das outras tarefas.
- Produces:
  - `search`: `pub const PREVIEW_CHARS: usize = 400`, `CONTEXT_CHARS: usize = 40`, `LATE_MATCH: usize = 300`, `MAX_MATCHES: usize = 20`, `SHORT_QUERY: usize = 2`.
  - `pub fn normalize(s: &str) -> String` (Decisões); `pub fn file_names(uris: &[String]) -> String` (o último segmento de cada `file://`, com `%XX` decodificado em UTF-8 com perda, separados por espaço).
  - `pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>>` (em **caracteres** do `text`, sem sobreposição, até `MAX_MATCHES`; a comparação é entre `normalize` de cada caractere do texto e `normalize(query)`, guardando de que caractere original veio cada pedaço normalizado, para o intervalo cair nos caracteres originais).
  - `pub fn preview_for(text: &str, query: Option<&str>) -> (String, Vec<[usize; 2]>)` (espaços colapsados; janela e "…" como nas Decisões; os intervalos em UTF-16 da prévia, só os que cabem nela).
  - `thumb`: `pub const THUMB_SIDE: u32 = 272`, `MAX_SIDE: u32 = 16_384`, `MAX_ALLOC: u64 = 512 << 20`; `#[derive(Debug)] pub struct Thumb { pub width: u32, pub height: u32, pub png: Vec<u8> }` (`width`/`height` da imagem original); `pub fn thumbnail(bytes: &[u8], mime: &str) -> Result<Thumb, String>` (só `image/png`, `image/jpeg`, `image/webp`; outro → `Err("sem miniatura para <mime>")`; `ImageReader` com `Limits`; lado maior da miniatura = `THUMB_SIDE`, nunca aumentando); `pub fn extension_for(mime: &str) -> &'static str` (`png`, `jpg`, `webp`, `gif`, `bmp`, `avif`, `tiff`, senão `img`); `pub fn sniff(bytes: &[u8]) -> Option<&'static str>` (o mime pela assinatura, por `image::guess_format`: PNG, JPEG, WebP, GIF, BMP, AVIF, TIFF; outro → `None`).
  - `testing::png_bytes(w: u32, h: u32) -> Vec<u8>` e `jpeg_bytes(w, h)` (um degradê gerado pelo `image`).

- [ ] **Passo 1: os testes.**
  - `tests/search.rs`:
    - `normalize_drops_case_and_accents`: `normalize("Ação CAFÉ São")` = `"acao cafe sao"`; `normalize("Ñandú")` = `"nandu"`.
    - `matches_ignore_case_and_accents`: `find_matches("Uma Ação, outra ação", "acao")` = `[4..8, 16..20]`; `find_matches("abc", "")` = `[]`.
    - `matches_are_utf16_ranges`: `preview_for("😀 Ação", Some("acao"))` = `("😀 Ação", vec![[3, 7]])` (o emoji conta 2 em UTF-16).
    - `preview_collapses_and_cuts`: `"a\n\n  b"` → `"a b"`; 1000 `x` → 400 caracteres; com `"y"` no caractere 900 e a consulta `y` → a prévia começa com `"…"` e o par cai em `[41, 42]`.
    - `short_query_uses_the_same_rule`: `find_matches("Pão", "ao")` = `[1..3]`.
    - `file_names_decode`: `["file:///home/u/relat%C3%B3rio.pdf", "file:///tmp/a%20b"]` → `"relatório.pdf a b"`.
  - `tests/thumb.rs`:
    - `png_thumbnail_keeps_aspect`: `thumbnail(&png_bytes(600, 400), "image/png")` → `width` 600, `height` 400, e a miniatura (decodificada) 272 × 181.
    - `jpeg_and_small_images`: `jpeg_bytes(100, 50)` → miniatura 100 × 50 (não aumenta).
    - `huge_dimensions_are_refused`: um PNG de 1 × 1 com o cabeçalho `IHDR` reescrito para 100 000 × 100 000 (o CRC recalculado no teste) → `Err`, em menos de 1 s.
    - `unknown_image_has_no_thumbnail`: `thumbnail(b"GIF89a", "image/gif")` → `Err` com "sem miniatura"; `extension_for("image/jpeg")` = `"jpg"`, `("image/x-foo")` = `"img"`.
    - `sniff_reads_the_signature`: `sniff(&png_bytes(2, 2))` = `Some("image/png")`, `sniff(&jpeg_bytes(2, 2))` = `Some("image/jpeg")`, `sniff(b"%PDF-1.7")` = `None` (o nome do arquivo não entra).
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-clipboard --test search --test thumb` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-clipboard --test search --test thumb` → 11 passed; o workspace inteiro verde. Se um número de teste se mostrar errado pela conta (não pelo código), pare e relate antes de mudá-lo.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: a busca sem acentos com o trecho em UTF-16 e a miniatura"`

---

### Tarefa 3: `lucernad` — o banco

**Files:**
- Create: `crates/lucernad-clipboard/src/store.rs`
- Modify: `Cargo.toml` (`rusqlite`, `sha2`), `crates/lucernad-clipboard/Cargo.toml`
- Test: `crates/lucernad-clipboard/tests/store.rs`

**Interfaces:**
- Consumes: `Kind`, `offers_for` (Tarefa 1); `normalize`, `file_names`, `preview_for`, `thumbnail`, `extension_for`, `png_bytes`, `jpeg_bytes` (Tarefa 2).
- Produces:
  - `pub enum NewClip { Text(String), Files(String), Image { mime: String, bytes: Vec<u8> } }` (`Files` com a `text/uri-list` crua).
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub enum Origin { Disk(PathBuf), Memory { files: PathBuf, disk: PathBuf } }` (a pasta do banco; na memória, a pasta dos arquivos e a do disco, que é apagada).
  - `pub struct Prepared` (opaco: kind, hash, texto, `norm`, mime, dimensões, `size`, `lines`, o nome do arquivo); `pub fn prepare(clip: NewClip, files_dir: &Path) -> Result<Prepared, String>` (função livre, **sem** o `Store`: hash, normalização, miniatura e, para imagem, a gravação de `<hash>.<ext>` e `<hash>.thumb.png` por arquivo temporário + `rename` na mesma pasta, pulando o que já existe); `pub fn wipe(dir: &Path) -> Result<(), String>` (apaga `history.db`, `-wal`, `-shm` e `images/`).
  - `#[derive(Debug, Clone, Default)] pub struct ListQuery { pub kind: Option<Kind>, pub query: Option<String>, pub offset: usize, pub limit: usize }`; `pub struct Page { pub items: Vec<serde_json::Value>, pub total: usize }` (os itens já no JSON do Formato).
  - `pub struct Store` com: `open(origin: Origin, limit: usize) -> Result<Store, String>` (cria a pasta com `lucerna_paths::create_private_dir`; disco: `history.db` + `images/`, apaga os órfãos de `images/`; memória: `wipe(disk)`, `:memory:` e apaga o que houver em `files/images/`); `origin(&self) -> &Origin`; `files_dir(&self) -> PathBuf` (para o `prepare`); `insert(&mut self, p: Prepared, now_ms: i64) -> Result<(i64, bool), String>` (só a transação: o id e se é novo; repetido: `used = now`, mesmo id e `pinned`; aplica o limite); `add(&mut self, clip: NewClip, now_ms: i64) -> Result<(i64, bool), String>` (`prepare` + `insert`, para os testes e a linha de comando); `touch(&mut self, id: i64, now_ms: i64) -> Result<bool, String>`; `remove(&mut self, id: i64) -> Result<bool, String>`; `pin(&mut self, id: i64, pinned: bool) -> Result<bool, String>`; `clear(&mut self, all: bool) -> Result<usize, String>`; `set_limit(&mut self, limit: usize) -> Result<(), String>`; `counts(&self) -> Result<(usize, usize), String>` (total e fixados); `list(&self, q: &ListQuery) -> Result<Page, String>`; `get(&self, id: i64) -> Result<Option<serde_json::Value>, String>`; `content(&self, id: i64) -> Result<Option<(Kind, Option<String>, Vec<u8>)>, String>` (o que o `offers_for` precisa); `move_to(self, origin: Origin) -> Result<Store, String>` (memória → disco: `wipe` do destino antes, `VACUUM INTO`, cópia + remoção dos arquivos; disco → memória: `wipe` do disco e começa vazio); `impl Drop for Store` (memória: apaga a pasta dos arquivos; disco: nada).
  - O limite: os não fixados acima de `limit` saem por `used` mais antigo, junto com os arquivos, na mesma transação (os arquivos depois do `COMMIT`).

- [ ] **Passo 1: os testes** (`tests/store.rs`; `disk()` = `Store::open(Origin::Disk(tmp.path().join("clipboard")), 100)`; `t(s)` = `NewClip::Text(s.into())`; `ids(page)` = os `id` da página).
  - `adds_and_lists_newest_first`: `a`, `b`, `c` em `now` 1, 2, 3 → `list` com `ids` `[c, b, a]`, `total` 3; o JSON de `a` com `kind` `"text"`, `preview` `"a"`, `size` 1, `lines` 1, `created` 1, `pinned` `false`, sem `matches`.
  - `duplicate_moves_to_top_keeping_id_and_pin`: `pin(a, true)`, `add(t("a"), 9)` → `(a, false)`; `a` com `used` 9 e `pinned` `true`; `counts` `(3, 1)`.
  - `pinned_come_first`: com `b` fixado, `ids` começa por `b`.
  - `limit_counts_only_unpinned_and_deletes_files`: `limit` 2, `a` fixado, três imagens (`png_bytes` de tamanhos diferentes) → 3 itens (a + 2 imagens), a imagem mais antiga e seus dois arquivos fora de `images/`; `set_limit(1)` tira mais uma.
  - `same_image_twice_is_one_item_and_one_file`: a mesma imagem duas vezes → um item, `images/` com 2 arquivos (`<hash>.png` e `<hash>.thumb.png`), o JSON com `width`, `height`, `thumb` existentes.
  - `files_item_lists_names`: `Files("file:///tmp/a%20b.txt\r\nfile:///tmp/c\r\n")` → `kind` `"files"`, `uris` com as duas, `preview` `"a b.txt, c"`; a busca `"a b"` o acha.
  - `search_ignores_accents_and_marks`: `"Reunião de ação"` e `"outra coisa"`; `query` `"ACAO"` → 1 item, `matches` `[[11, 15]]`; `query` `"ão"` (curta) → o primeiro, `matches` `[[5, 7], [13, 15]]`; `query` `"x"` → 0.
  - `fts_syntax_is_literal`: `query` `"a\"b OR c*"` e `"NOT"` → sem erro (0 itens se nada casa); um texto `NOT here` é achado por `"not"`.
  - `pagination_and_kind_filter`: 120 textos e 2 imagens → `offset` 100, `limit` 50 → 22 itens, `total` 122; `kind` `Image` → `total` 2; com `query`, imagens nunca entram.
  - `orphans_are_removed_on_open`: um arquivo `images/zzz.png` sem item → some ao reabrir; os do banco ficam.
  - `memory_store_never_touches_disk`: `Origin::Memory { files: rt, disk: d }` com uma imagem → arquivos só em `rt/images`; depois do `drop`, `rt/images` vazio; nenhum `history.db` em lugar nenhum do `tmp`.
  - `memory_open_wipes_the_disk`: um disco com itens e imagens em `d` → `Store::open(Memory { files: rt, disk: d })` → `d` sem `history.db*` nem `images/*`, e a memória vazia.
  - `back_to_disk_over_leftover_files`: um `history.db` e um `-wal` soltos em `d` (de uma queda) → `move_to(Disk(d))` da memória com dois itens → `Ok`, e reabrir mostra os dois.
  - `prepare_needs_no_store`: `prepare(Image { png_bytes(2000, 2000) }, dir)` sem nenhum `Store` aberto → os dois arquivos em `dir/images`; `insert` dele num `Store` grava o item sem tocar nos arquivos de novo (o `mtime` igual).
  - `persist_off_then_on_moves_items_and_files`: disco com texto e imagem → `move_to(Memory { files: rt, disk: d })` → disco sem `history.db*` nem `images/*`, a memória vazia; dois itens novos (um com imagem) → `move_to(Disk(d))` → `list` com os dois, os arquivos em `d/images` e nenhum em `rt/images`; reabrir o disco mostra os dois (os diretórios do teste ficam em `tmp` e num segundo `TempDir` sob `/dev/shm` quando existir, para pegar o `EXDEV`).
  - `survives_reopen`: fechar só com `drop` e reabrir → os itens e o `pinned` lá.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-clipboard --test store` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces e o esquema das Decisões.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-clipboard --test store` → 16 passed; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: o histórico em SQLite com busca trigram, imagens e limite"`

---

### Tarefa 4: `lucernad` — o Wayland, atrás do `Backend`

**Files:**
- Create: `crates/lucernad-clipboard/src/{backend,wayland}.rs`; em `src/testing.rs`, `FakeBackend`; `examples/selecao.rs`; `tests/isolado.sh`
- Modify: `Cargo.toml` (as três do Wayland), `crates/lucernad-clipboard/Cargo.toml`
- Test: `crates/lucernad-clipboard/tests/{backend,sonda}.rs`

**Interfaces:**
- Consumes: `socket_path`, `OWN_MIME` (Tarefa 1).
- Produces:
  - `backend`: `#[derive(Debug, PartialEq)] pub enum Event { Selection { offer: u64, mimes: Vec<String> }, Cleared, Cancelled, Disconnected(String) }` (`offer` cresce a cada seleção; `Cancelled` = a nossa fonte foi substituída).
  - `#[derive(Clone)] pub struct Source { pub offers: Vec<(String, Arc<[u8]>)> }`.
  - `pub trait Control: Send { fn receive(&self, offer: u64, mime: &str) -> Result<std::io::PipeReader, String>; fn offer(&self, source: Source); }` (`receive` de uma oferta que já não é a atual → `Err("a seleção mudou")`).
  - `pub struct Session { pub events: tokio::sync::mpsc::UnboundedReceiver<Event>, pub control: Box<dyn Control> }`; largar a `Session` fecha a conexão.
  - `pub trait Backend: Send + Sync + 'static { fn open(&self, socket: &Path) -> Result<Session, String>; }` (bloqueante; o módulo usa o `detached`).
  - `wayland`: `pub const CONNECT_TIMEOUT: Duration` (2 s); `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Protocol { Ext, Wlr }`; `pub struct Wayland { pub timeout: Duration, pub only: Option<Protocol> }` com `system()` (`CONNECT_TIMEOUT`, `None`); `impl Backend for Wayland`. Abertura: `UnixStream::connect` + `Connection::from_socket`, `registry_queue_init`, o gerente (o `ext` antes) e o primeiro `wl_seat`, um `roundtrip` dentro do prazo. O `receive`: `std::io::pipe()`, `offer.receive(mime, writer.as_fd())`, `conn.flush()` e **largar a ponta de escrita** na hora (sem isso, o EOF nunca chega). Erros: os textos de conexão, prazo, falta do protocolo e queda do Formato. **Armadilha conferida na sonda:** o `Dispatch` do device precisa de `event_created_child!` para o `data_offer` (sem ele, pânico ao primeiro evento).
  - `testing::FakeBackend` (`Clone`): `new()`, `failing(msg: &str)`, `panicking()`, `copy(&self, mimes: &[&str], data: &[(&str, &[u8])]) -> u64` (manda `Selection` e guarda o que cada tipo devolve), `copy_stuck(&self, mimes: &[&str]) -> u64` (o `receive` devolve um pipe que nunca fecha), `cancel(&self)` (outro app copiou: `Cancelled` se havia fonte nossa), `disconnect(&self, why: &str)`, `paste(&self, mime: &str) -> Option<Vec<u8>>` (o que a nossa fonte serve), `owned(&self) -> Option<Vec<String>>` (os tipos da nossa fonte), `reads(&self) -> usize` (quantos `receive`), `sessions()`, `opened()`, `sockets(&self) -> Vec<PathBuf>` (os sockets pedidos ao `open`); o `receive` do fake escreve os bytes numa thread pelo `std::io::pipe`. `testing::isolated()` (a trava da sonda, abaixo).
  - `examples/selecao.rs`: `selecao oferecer MIME… < dados` (lê o stdin, oferece os bytes em cada tipo, imprime `pronto` ao virar dono e sai com 0 quando outra cópia a substitui), `selecao ler MIME` (imprime os bytes do tipo na seleção atual; sem ele, sai com 1) e `selecao tipos`; exibição pelo `WAYLAND_DISPLAY` e `XDG_RUNTIME_DIR`.

**`tests/isolado.sh`** (`set -euo pipefail`, o cabeçalho com o uso e o `docker run` abaixo): recusa rodar fora de container ("isolado.sh: só roda dentro de um container (sobe um compositor de teste)"), como o do espectro; cria um `XDG_RUNTIME_DIR` temporário (0700); sobe `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway -c /dev/null` com `setsid`; espera até 10 s o socket `wayland-1`; copia o repositório (montado só para leitura) por `tar`, sem `target` nem `.git`; roda nele `cargo build --locked -p lucernad-clipboard --example selecao` e `cargo test -p lucernad-clipboard --test sonda --locked -- --ignored --test-threads=1` com `LUCERNA_SELECAO` = o caminho do exemplo; encerra o sway num `trap`. Exporta `LUCERNA_ISOLADO=1` só para o `cargo test`. Roda a sonda em duas chamadas: `-- --ignored --test-threads=1 --skip compositor_drop` e, por último, `-- --ignored --exact compositor_drop_is_an_event` (o que mata o sway).

- [ ] **Passo 1: os testes.**
  - `tests/backend.rs` (rodam no CI):
    - `fake_copy_reads_through_a_pipe`: `copy(&["text/plain"], &[("text/plain", b"oi")])` → `Selection { offer: 1, mimes }`; `read_pipe(control.receive(1, "text/plain")?, …)` = `b"oi"`; `receive(0, …)` → `Err("a seleção mudou")`.
    - `fake_serves_and_cancels`: `control.offer(Source { offers: vec![("text/plain", "x")] })` → `owned()` = `["text/plain"]`, `paste("text/plain")` = `b"x"`; `cancel()` → `Event::Cancelled` e `owned()` `None`.
    - `fake_counts_and_fails`: `sessions()`/`opened()` como no espectro; `failing("e")` → `open` com `Err("e")`; `disconnect("y")` → `Disconnected("y")` e o canal fecha.
    - `sonda_refuses_outside_the_container`: sem `LUCERNA_ISOLADO`, `std::panic::catch_unwind(isolated)` → `Err` (o ajudante fica em `testing::isolated`, para o teste do CI e a sonda usarem o mesmo).
    - `wayland_without_compositor_is_an_error`: `Wayland::system().open(Path::new("/inexistente/wayland-9"))` → `Err` começando por `"Não consegui conectar ao Wayland (/inexistente/wayland-9)"`, em menos de 3 s.
  - `tests/sonda.rs` (todos `#[ignore = "só no compositor isolado (tests/isolado.sh)"]`; cada teste começa por `isolated()`, que entra em pânico com "sonda.rs: só roda pelo tests/isolado.sh, num container" a menos que haja `LUCERNA_ISOLADO=1` **e** `/.dockerenv` ou `/run/.containerenv`, para um `cargo test -- --ignored` no host nunca alcançar o KWin do usuário; `wl_copy(args, stdin)` e `wl_paste(args)` chamam as ferramentas do container; `socket()` = `$XDG_RUNTIME_DIR/wayland-1`):
    - `sees_a_real_copy_and_reads_it`, para `Protocol::Ext` e `Protocol::Wlr` (`only`): `wl-copy "olá mundo"` → em 2 s um `Selection` com `text/plain;charset=utf-8` entre os tipos; a leitura dá `"olá mundo"`; `wl-copy --type image/png < <png_bytes(64, 64)>` → `Selection` com `image/png` e os mesmos bytes.
    - `serves_what_it_offers`: `offer` com os tipos de texto de `offers_for(Text, …, b"colado", 5)` → `wl-paste --list-types` lista os seis; `wl-paste --no-newline` = `colado`; `wl-paste --type application/x-lucernad-item` = `5`; depois `wl-copy outro` → `Event::Cancelled` em 2 s.
    - `compositor_drop_is_an_event` (o último, sozinho): com a sessão aberta, o teste mata o sway → `Disconnected` em 2 s.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-clipboard --test backend` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces (a thread da sessão como nas Decisões; o `send` em thread própria).
- [ ] **Passo 4: verificar.**
  - `cargo test -p lucernad-clipboard --test backend` → 5 passed; o workspace inteiro verde.
  - A sonda, no container isolado (sem nenhum socket do host; o `setcap -r` porque o sway do Arch tem capabilities de arquivo que o container recusa):

    ```sh
    docker run --rm -v "$PWD:/src:ro" -v ~/develop/rustup/toolchains/1.95-x86_64-unknown-linux-gnu:/tc:ro archlinux:latest \
      bash -c 'pacman -Syu --noconfirm --needed sway wl-clipboard libcap gcc > /dev/null && setcap -r /usr/bin/sway \
        && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-clipboard/tests/isolado.sh'
    ```

    Critério: 4 passed (os dois protocolos contam como dois). Se o sway sem dispositivos de entrada não entregar a seleção, tentar `WLR_HEADLESS_OUTPUTS=1` e anotar no relatório.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: o controle de dados do Wayland, atrás do trait Backend"`

---

### Tarefa 5: `lucernad` — o módulo `clipboard`

**Files:**
- Modify: `crates/lucernad-clipboard/src/lib.rs` (o módulo), `src/testing.rs` (`fake_config`, `quiet_config`), `Cargo.toml` (`lucernad-core`, `lucerna-service`, `lucerna-paths`, `tokio`, `serde_json`), `tests/sonda.rs` (o caso do módulo), `tests/isolado.sh`
- Test: `crates/lucernad-clipboard/tests/module.rs`

**Interfaces:**
- Consumes: Tarefas 1–4; `lucernad_core::{detached, Detached, supervise, publish_changed, ModuleTask}`; do SDK, `lucerna_service::{ServerBuilder, Topic, RpcError, WorkTracker, WorkGuard}`, `lucerna_paths::{Env, AppPaths}`; nos testes, `lucerna_ipc::Client`.
- Produces:
  - `pub const OPEN_TIMEOUT: Duration` (2 s), `CONFIGURE_WAIT` (2 s), `RETRY` (5 s), `DEFAULT_LIMIT: usize = 100`, `MAX_LIMIT: usize = 1000`, `LIST_MAX: usize = 200`.
  - `pub struct ClipboardConfig { pub backend: Arc<dyn Backend>, pub data_dir: Option<PathBuf>, pub runtime_dir: Option<PathBuf>, pub display_env: Option<String>, pub configure_wait: Duration, pub retry: Duration }` com `pub fn system() -> ClipboardConfig` (`Wayland::system()`, os caminhos do Formato pela `lucerna-paths`, `WAYLAND_DISPLAY` do ambiente, os prazos acima). `runtime_dir` é o `XDG_RUNTIME_DIR` (para o `socket_path` e para a pasta sem persistência).
  - `#[derive(Clone)] pub struct ClipboardTopics { pub history: Topic, pub status: Topic }`.
  - `pub fn register(server: &mut ServerBuilder, cfg: ClipboardConfig, work: WorkTracker) -> Clipboard` (os tópicos comuns `clipboard.history` (`null`) e `clipboard.status` (`{"available":false,"error":null}`) e os nove métodos do Formato); `pub struct Clipboard { pub topics: ClipboardTopics, /* privado */ }` com `pub fn spawn(self) -> ModuleTask` (o laço e o `supervise` com `"da área de transferência"` e `{"available":false,"error":"O módulo da área de transferência parou por um erro interno."}`).
  - `testing::fake_config(backend: FakeBackend, dir: &Path) -> ClipboardConfig` (`data_dir` = `dir/data`, `runtime_dir` = `dir/run`, `display_env` `Some("wayland-teste")`, `configure_wait` 200 ms, `retry` 100 ms); `testing::quiet_config(dir: &Path) -> ClipboardConfig` (`FakeBackend::new()`), para o binário.

O laço (o que as assinaturas não determinam):

```
estado: store: Option<Store>; cfg (limit, persist, display) guardado num watch; owned: Option<(id, WorkGuard)>; seq de cópias

configuração: o primeiro configure abre o Store (Disk ou Memory) e publica history;
  sem configure, CONFIGURE_WAIT depois do primeiro assinante ou do primeiro método → padrão
  configure seguinte: persist mudou → store.move_to(…); limit mudou → set_limit; publica history se mudou

loop {
    espera (history.subscribers() > 0 e configurado) ou um pedido de copy pendente
    socket = socket_path(display_env ou cfg.display, runtime_dir)   Err(e) → status {false, e}; espera mudança; continue
    sessão = detached(backend.open(socket), OPEN_TIMEOUT)            erro/prazo → status; espera retry; continue
    status {available:true, error:null}
    loop { select! {
        Selection{offer, mimes} → choose(mimes):
            Secret  → log "lucernad: cópia sensível ignorada"
            Own     → store.touch(owned.id)
            Read{kind,mime} → n = ++seq; spawn_blocking: read_pipe(receive(offer,mime), kind.max_size(), agora+READ_TIMEOUT)
                              → resultado volta ao laço com n
            Nothing → nada
        resultado n Ok(bytes) → spawn_blocking: prepare(clip, files_dir) sem o lock; depois, na ordem de n (um resultado
                                mais novo que chegou antes espera o anterior ou o prazo dele), insert com o lock; publica history
        resultado Err(e)      → log com o tipo e o erro
        pedido copy(id)       → spawn_blocking: store.content(id) → control.offer(Source { offers_for(…) }); store.touch;
                                owned = (id, work.hold()); responde {}
        Cancelled             → owned = None
        Cleared               → nada
        Disconnected(e) | None → status {false, "O compositor fechou a conexão: e."}; owned = None; espera retry; sai
        history sem assinante e owned None → sai
    } }
    drop(sessão)
}
```

Os métodos `list`/`get`/`remove`/`pin`/`clear`/`copyImage` usam o `Store` por um `Arc<std::sync::Mutex<Option<Store>>>` dentro de `spawn_blocking`; cada mudança aumenta a `revision` (contador do processo) e publica `history` com `publish_changed`. O `copyImage` lê o arquivo e acha o mime por `sniff` em `spawn_blocking`; com `store`, `prepare` + `insert` como uma cópia observada; sem `store`, a fonte só com o mime da imagem. O `copy` (e o fim do `copyImage`) vai ao laço por um `mpsc` com resposta `oneshot`: sem sessão, o laço abre uma; se não conseguir, `1003`. O `status` só é publicado quando muda. Ao parar (o `ModuleTask` largado, que aborta o laço), o `Drop` do `Store` em memória apaga os arquivos.

- [ ] **Passo 1: os testes** (`tests/module.rs`, com os ajudantes do `tests/module.rs` do espectro: `serve(cfg, work)` guardando o `ModuleTask`, `client(&path)`, `wait_until`, e `wait_seq` do `lucernad-system`; `subscribed(&c)` assina `clipboard.history`; `configure(&c, json)`; `list(&c, json) -> Value`; `text(&b, s)` = `b.copy(&["text/plain;charset=utf-8"], &[("text/plain;charset=utf-8", s)])`):
  - `handshake_lists_topics_and_methods`: `topics` `["clipboard.history", "clipboard.status"]`, `streams` `[]`, `methods` com os nove nomes.
  - `params_are_validated`: um caso por texto de `-32602` do Formato (ex.: `configure {"limit":0,"persist":true}`, `{"limit":5,"persist":"sim"}`, `list {"offset":-1,"limit":10}`, `list {"kind":"video","offset":0,"limit":10}`, `pin {"id":"1","pinned":true}`, `copyImage {"path":"rel.png"}`, `copyImage {"path":"/a.png","store":1}`), cada um com a mensagem exata.
  - `nothing_observes_before_configure`: com assinante e sem `configure`, `opened()` `0` aos 100 ms e `1` depois do `configure`.
  - `default_configuration_after_the_wait`: sem `configure`, assinante → `opened()` `1` em 400 ms (o `configure_wait` de 200 ms) e `data/history.db` existe.
  - `copies_become_history`: texto, imagem `png_bytes(64, 64)` e arquivos → `history` chega a `count` 3; `list` com os três `kind`; o item de imagem com `thumb` existente.
  - `secret_is_never_read_nor_stored`: `copy(&["text/plain", "x-kde-passwordManagerHint"], …)` → depois de 300 ms, `count` igual e nenhuma leitura no fake (`reads()` `0`); o texto não aparece em `history.db` (busca dos bytes no arquivo e no `-wal`).
  - `own_copy_only_moves_to_top`: `copy {id: a}` → `owned()` com `OWN_MIME`; o fake repete a seleção com os tipos da fonte → `count` igual e `a` no topo.
  - `owned_selection_outlives_the_subscriber`: com `work = WorkTracker::new()`, `copy {id}` → `work.pending()` `1`; `unsubscribe` → `sessions()` continua `1`; `cancel()` → `pending()` `0` e `sessions()` `0` em 200 ms.
  - `copy_without_subscriber_opens_a_session`: sem assinante, `configure` e `copy {id}` de um item já gravado (de uma sessão anterior) → `sessions()` `1`, `paste("text/plain")` = o texto.
  - `fast_copies_keep_their_order`: `copy_stuck` seguido de dois textos `x` e `y` → depois do prazo de leitura, `list` = `[y, x]` (o preso descartado), em menos de `READ_TIMEOUT` + 1 s.
  - `stuck_offer_does_not_block_the_next_copy`: com um `copy_stuck` pendente, um `list` responde em menos de 100 ms.
  - `first_configure_without_persist_wipes_the_disk`: um `data/history.db` com itens de antes (aberto e largado pelo teste) → `configure {"limit":100,"persist":false}` → `data/` sem `history.db*` nem `images/*`, `count` 0.
  - `aborting_the_module_clears_memory_files`: sem persistência, uma imagem copiada → `run/lucerna/clipboard/images` com arquivos; largar o `ModuleTask` → vazio em 500 ms.
  - `list_stays_fast_while_a_big_image_is_saved`: `copy` de `png_bytes(4000, 4000)` e, logo depois, um `list` → responde em menos de 100 ms, antes de o item da imagem aparecer.
  - `persist_off_keeps_disk_clean`: `configure {"limit":100,"persist":false}` antes de assinar, um texto e uma imagem → nada em `data/`; os arquivos em `run/lucerna/clipboard/images`; religar a persistência → os dois em `data/` e `run/…/images` vazio.
  - `limit_change_trims`: 5 textos, `configure` com `limit` 2 → `count` 2 e `revision` nova.
  - `clear_and_pin`: `pin {id, pinned: true}`, `clear {}` → só o fixado; `clear {"all":true}` → `count` 0; `remove`/`pin` de id inexistente → `{}`; `copy`/`get` de id inexistente → código `1001`.
  - `missing_display_is_state`: `display_env` `None` e `configure` sem `waylandDisplay` → status `{"available":false,"error":"Sem WAYLAND_DISPLAY: o lucernad não sabe qual sessão observar."}`; `configure` com `"waylandDisplay":"wayland-7"` → `sockets()` termina em `run/wayland-7` e `available` `true`.
  - `compositor_drop_becomes_state_and_retries`: `disconnect("x")` → status com `"O compositor fechou a conexão: x."`; em 1 s `opened()` `2` e `available` `true`.
  - `copy_image_from_disk`: `copyImage {path}` de um PNG salvo como `dir/foto.jpg` (a extensão mente) → `{id}`, `owned()` `["image/png", OWN_MIME]`, o item com `mime` `image/png`; `copyImage {path, store: false}` de outro PNG → `{id: 0}`, `owned()` `["image/png"]` e `count` igual; um caminho inexistente e um arquivo de texto → `1002`.
  - `panic_marks_the_module_unavailable`: `FakeBackend::panicking()` → status com o erro interno; um cliente novo ainda faz o aperto de mão.
  - Em `tests/sonda.rs`, `module_keeps_real_copies`: o módulo com `Wayland::system()` e diretórios temporários, assinante e `configure`; `wl-copy "Ação real"`, `wl-copy --type image/png` de um PNG, `wl-copy --type image/jpeg` de um JPEG, `wl-copy --type text/uri-list "file:///tmp/a.txt"`, e `printf segredo | $LUCERNA_SELECAO oferecer text/plain x-kde-passwordManagerHint` → `count` 4 e a busca `segredo` vazia; `copy` do PNG → `wl-paste --type image/png` igual aos bytes; `copy` do texto e `wl-copy outro` → o `WorkTracker` volta a `0`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-clipboard --test module` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces e o laço.
- [ ] **Passo 4: verificar.** `TMPDIR=/tmp cargo test -p lucernad-clipboard --test module` → 21 passed, 5 vezes seguidas sem falha intermitente; o workspace inteiro verde; a sonda da Tarefa 4 de novo → 5 passed.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: o módulo, depois do configure, com a seleção segurada"`

---

### Tarefa 6: `lucernad` — o módulo no binário

**Files:**
- Modify: `crates/lucernad/{Cargo.toml,src/lib.rs,src/main.rs}`, `crates/lucernad/tests/{common/mod.rs,protocol.rs,process.rs}`, `README.md`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `lucernad_clipboard::{register, ClipboardConfig, ClipboardTopics}`, `testing::quiet_config`; `ServiceBuilder::work()`; `lucerna_cli::{CliCommand, Call}`.
- Produces:
  - `Cargo.toml`: `default = ["system", "spectrum", "clipboard"]`, `clipboard = ["dep:lucernad-clipboard", "dep:lucernad-core", "dep:serde_json"]` (comentário `# O módulo da área de transferência`); `description = "O daemon do Lucerna: um binário com um módulo por área (system, spectrum e clipboard)."`.
  - `Modules` ganha `#[cfg(feature = "clipboard")] pub clipboard: ClipboardConfig` (`real()`: `ClipboardConfig::system()`); `Lucernad` ganha `clipboard: ClipboardTopics` e `clipboard_task: ModuleTask`; o `lucernad()` chama `service.work()` e o passa ao `register`; as duas listas `any(feature = …)` ganham `"clipboard"`; o `main` guarda `_clipboard_task`; a descrição do `cli()` vira "O daemon do Lucerna: os dados do sistema, o espectro do som e o histórico da área de transferência para o shell.".
  - Com `clipboard`, dois `CliCommand`: `historico` (`about` "Mostra os últimos itens da área de transferência (com BUSCA, só os que casam)."; argumento opcional `busca`) → `clipboard.list {query?, offset: 0, limit: 20}`, `render` = uma linha `"{id}  {kind}  {preview}"` por item (imagem: `"{width}×{height} {mime}"`), "Nada no histórico." se vazio; `copiar` (`about` "Põe um item do histórico na área de transferência."; argumento `id` obrigatório; não número → `Err("ID precisa ser um número")`) → `clipboard.copy {id}`.
  - CI: o clippy ganha `cargo clippy -p lucernad --no-default-features --features clipboard --all-targets --locked -- -D warnings`.
  - `README.md`: "Situação" com os três módulos; em **Módulos**, a seção `clipboard` no formato das outras (o que faz e quando roda: depois do primeiro `configure` e com assinante, ou segurando a seleção; a tabela dos dois tópicos; os métodos numa lista; onde mora o histórico, sem persistência no `$XDG_RUNTIME_DIR`; o sensível nunca entra); em **Linha de comando**, `lucernad historico [busca]` e `lucernad copiar ID`; em **Desenvolvimento**, a sonda do `tests/isolado.sh` com o `docker run` da Tarefa 4.

- [ ] **Passo 1: os testes.**
  - `tests/common/mod.rs`: `Modules { …, clipboard: quiet_config(&dir) }`, guardando o terceiro `ModuleTask`.
  - `protocol.rs`, `the_daemon_serves_the_clipboard_module`: `hello().topics` contém `"clipboard.history"` e `"clipboard.status"`, `hello().methods` contém `"clipboard.list"`; `clipboard.configure {"limit":100,"persist":false}` → `{}` e `clipboard.list {"offset":0,"limit":5}` → `{"items":[],"total":0}`.
  - `process.rs`, em `cli_help_status_and_exit_codes`: `v["clipboard.status"]["available"]` = `false` (sem Wayland no ambiente limpo); `lucernad historico` → código 0 e "Nada no histórico." (depois da espera de 2 s do padrão); `lucernad copiar abc` → código de uso, stderr com "ID precisa ser um número"; `lucernad copiar 99` → código diferente de 0, stderr com "não existe mais".
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad` → falha.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** O workspace inteiro verde, com os cinco clippy das Global Constraints; `grep -rnE "process::Command|tokio::process" crates/*/src` → nada.
- [ ] **Passo 5: commit.** `git commit -m "lucernad: o módulo clipboard no binário, os subcomandos historico e copiar e o README"`

---

### Tarefa 7: `lucernad` — conferência na máquina real, PR e merge

Roda depois das Tarefas 8–9. **O usuário copia e cola**; o controlador pede cada coisa, espera a confirmação e só então roda a conferência. Nada vai para o `XDG_DATA_HOME` do usuário.

**Files:** nenhum, salvo correções (cada uma com teste e commit próprio).

**Interfaces:**
- Consumes: o binário da Tarefa 6; o shell da `feat/area-de-transferencia` (Tarefas 8–9).
- Produces: `<LUCERNAD_REV>` = o SHA da `main` do `lucernad` depois do merge, para a Tarefa 10.

- [ ] **Passo 1: o daemon à parte do instalado** (a sessão do usuário é o KWin 6.8, só com o `ext-data-control` v1; o `/usr/bin/lucernad` dele continua no socket do systemd):

  ```sh
  cargo build --release --locked -p lucernad
  R=$(mktemp -d /tmp/lucernad-conf.XXXXXX) D=$(mktemp -d /tmp/lucernad-dados.XXXXXX)
  HOST_WL=$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY   # o socket da sessão, absoluto, antes de trocar o XDG_RUNTIME_DIR
  run() { WAYLAND_DISPLAY=$HOST_WL XDG_RUNTIME_DIR=$R XDG_DATA_HOME=$D LUCERNA_IDLE_TIMEOUT_MS=${IDLE:-600000} target/release/lucernad "$@"; }
  run serve & SERVE=$!; run watch clipboard.history > $R/historico.log & WATCH=$!
  ```

  Critério: em 3 s, `run --json status` com `clipboard.status.available` `true`.
- [ ] **Passo 2: os tipos.** O usuário copia um texto com acentos e emoji (Kate), uma captura (Spectacle, PNG), uma foto JPEG (do Dolphin ou do navegador) e dois arquivos no Dolphin. Critério: `run historico` mostra os quatro, com o tipo certo (`files` com os dois nomes); `ls $D/lucerna/clipboard/images` com cada imagem e a `.thumb.png`; `run historico acao` acha o texto.
- [ ] **Passo 3: a senha.** O usuário cria no gerenciador de senhas dele (o KeePassXC ou o que usar, que marque `x-kde-passwordManagerHint`) uma entrada **só para o teste**, com a senha `lucerna-teste-senha-123`, e a copia de lá. Nunca se pede nem se busca uma senha real. Critério: nenhum item novo no `historico.log` e `run historico lucerna-teste-senha` vazio; depois o usuário apaga a entrada. Se o gerenciador não marcar a cópia, anotar no PR qual é e seguir (não é falha do módulo).
- [ ] **Passo 4: colar de volta.** `run copiar <id>` de cada tipo; o usuário cola: o texto no Kate, a imagem no Kate/GIMP/LibreOffice, os arquivos numa pasta do Dolphin (copia os arquivos). Critério: os quatro colam; o item colado sobe ao topo sem duplicar.
- [ ] **Passo 5: a seleção segurada.** Antes, o controlador pergunta se o Klipper do KDE está com "Impedir área de transferência vazia" ligado: ele pode tomar a seleção logo depois do `copiar` (o daemon recebe `cancelled` e solta) ou mantê-la depois de o daemon sair, mascarando o resultado; se estiver, o usuário o desliga só para este passo. `kill $WATCH $SERVE`; `IDLE=5000 run serve & SERVE=$!`; `run copiar <id do texto>`; 20 s depois, `kill -0 $SERVE` ainda dá certo e o usuário ainda cola o texto; o usuário copia outra coisa → `kill -0 $SERVE` falha em até 10 s (saiu sozinho).
- [ ] **Passo 6: o histórico sobrevive.** `run serve & SERVE=$!` de novo → `run historico` com os mesmos itens e fixados.
- [ ] **Passo 7: o painel, no Hyprland do container.** No `lucerna-shell` (branch `feat/area-de-transferencia`): `dev/test.sh up`; `docker cp target/release/lucernad lucerna-polish:/usr/bin/lucernad` e `docker cp target/release/examples/selecao lucerna-polish:/usr/bin/lucernad-selecao` (depois de `cargo build --release --locked -p lucernad-clipboard --example selecao`); `dev/test.sh run 'pkill -x lucernad; setsid -f env LUCERNA_IDLE_TIMEOUT_MS=86400000 lucernad serve'` (só pelo `run`: Global Constraints); conferir o `WAYLAND_DISPLAY` do processo pelo `/proc/<pid>/environ` (Global Constraints) antes de qualquer cópia; abrir a janela do container para o usuário. O usuário copia no kitty e no foot do container, abre o `Super+V`, busca (o trecho marcado), usa o filtro "Arquivos" (depois de `dev/test.sh run 'printf "file:///etc/hostname" | lucernad-selecao oferecer text/uri-list &'`), escolhe um item com Enter (cola no kitty, pelo Ctrl+Shift+V), recarrega o shell (`dev/test.sh restart`) e cola de novo com Ctrl+Shift+V. Critério: o usuário aprova; `dev/test.sh errs` sem avisos novos.
- [ ] **Passo 8: PR.** `kill $SERVE` e apagar `$R` e `$D` (e o usuário religa o Klipper, se desligou); `git push -u origin feat/clipboard`; `gh pr create --repo lucerna-desktop/lucernad --base main --title "lucernad: o módulo da área de transferência" --body <resumo; como foi testado (a sonda isolada nos dois protocolos, os passos 2–7 com o que se viu); as linhas "Para o PR" da tabela de decisões: o valor do passwordManagerHint ignorado, os campos lines e pinned, o copyImage com store, os métodos esperando o configure; o tipo próprio application/x-lucernad-item; o gerenciador de senhas conferido e o que o Klipper fez>`. **Controlador:** revisão final da branch (Opus), CI verde, `gh pr merge <n> --repo lucerna-desktop/lucernad --merge`; anote `<LUCERNAD_REV>` (`git fetch && git rev-parse origin/main`); com o usuário, reinstale o `/usr/bin/lucernad` (a memória "lucernad instalado no sistema").

---

### Tarefa 8: `lucerna-shell` — `Clipboard.qml` como adaptador

Repositório `lucerna-shell`, branch `feat/area-de-transferencia` (Global Constraints).

**Files:**
- Modify: `services/Clipboard.qml` (reescrito), `services/Capture.qml:120-122` (`copyImage`), `tests/clipboard.test.js` (reescrito), `tests/coverage.json`
- Delete: `services/scripts/clipboard-watch.sh`

**Interfaces:**
- Consumes: `Lucernad.client` (`want`, `call`, `connected`, `received`, `offered`); o Formato.
- Produces (o singleton `Clipboard`):
  - Propriedades: `property var client: Lucernad.client` (não `readonly`: os testes trocam por um cliente da fábrica e restauram); `property bool watching: false`, `property int limit: 100`, `property bool persist: true` (os `Binding` do `ClipboardState` ficam); `property bool moduleAvailable: false`; `property string error: ""`; `readonly property bool available: client.connected && moduleAvailable`; `property int revision: -1`; `property int count: 0`; `property int pinned: 0`; `property var items: []`; `property int total: 0`; `property string kind: "all"`; `property string query: ""`; `readonly property int pageSize: 50`; `property int token: 0`; `property bool loading: false` (um `fetch` de troca, com `append` falso, em voo).
  - Funções: `wantedTopics(watching: bool): var` (`["clipboard.status"]`, mais `"clipboard.history"` com `watching`); `sync(): void` (`client.want("clipboard", …)`); `configureParams(limit: int, persist: bool, display: string): var` (`{limit, persist}` e `waylandDisplay` só se `display` não vazio); `configure(): void` (só conectado; manda antes do `sync`, para o daemon ter o `configure` antes do assinante); `listParams(kind: string, query: string, offset: int, limit: int): var` (sem `kind` para `"all"`, sem `query` vazia ou só espaços); `load(kind: string, query: string): void` (guarda e chama `fetch(0, pageSize, false)`); `more(): void` (só se `!loading` e `items.length < total`: `fetch(items.length, pageSize, true)`); `fetch(offset: int, limit: int, append: bool): void` (troca: `token` novo e `loading`; soma: guarda o `token` de agora, sem trocá-lo; `client.call("clipboard.list", …, r => applyPage(t, append, r))`); `applyPage(t: int, append: bool, response: var): void` (ignora token velho e erro; `Array.from(response.result.items)`; troca, ou soma pulando os `id` que já estão; `total`; a troca zera `loading`); `apply(topic: string, data: var): void`; `applyHistory(data: var): void` (`count`, `pinned`; `revision` nova → `fetch(0, Math.max(pageSize, items.length), false)`); `applyStatus(data: var): void` (`moduleAvailable`, `error`); `connectionChanged(connected: bool): void` (caída: `moduleAvailable` falso, `items` `[]`, `revision` −1; nos dois casos `configure()` e `sync()`); `copy(id: int): void`, `remove(id: int): void`, `pin(id: int, pinned: bool): void`, `clear(all: bool): void`, `copyImage(path: string): void` (os métodos do Formato, sem esperar resposta; `copyImage` manda `{path, store: watching}`, e o `Capture.copyImage` passa a chamar este).
  - Ligações: `onWatchingChanged: sync()`; `onLimitChanged`/`onPersistChanged: configure()`; `Connections` no `client` (`onReceived` → `apply`, `onConnectedChanged` → `connectionChanged(client.connected)`); `Component.onCompleted: { configure(); sync(); }`. O `display` vem de `Quickshell.env("WAYLAND_DISPLAY") ?? ""`. O colar sozinho (`paste`, `terminals`, `pasteDelay`, o `hyprctl`) fica como está.
  - Saem: `decode`, `makeId`, `add`, `trim`, `forget`, `entries`, `start`, `started`, `modified`, `imageDir`, `historyFile`, `script`, os `Process` do `wl-copy` e do `wl-paste`, o `FileView`, o `saveDelay`.
  - Cabeçalho: "O histórico da área de transferência, vindo do módulo clipboard do lucernad: o daemon observa, guarda e copia de volta; aqui só os tópicos, a página que o painel mostra e o colar sozinho (que depende do Hyprland). Sem o daemon, `available` fica falso."
  - `tests/coverage.json`: `"services/Clipboard.qml": ["wantedTopics", "sync", "configureParams", "configure", "listParams", "load", "more", "fetch", "applyPage", "apply", "applyHistory", "applyStatus", "connectionChanged", "copy", "remove", "pin", "clear", "copyImage"]`.

- [ ] **Passo 1: os testes** (`tests/clipboard.test.js`, cabeçalho dizendo o que testa; guarda e restaura `client`, `watching`, `items`, `total`, `moduleAvailable`, `revision`, `count`, `pinned`; `wired()` = um cliente de `t.lucernaClient()` com o hello respondido oferecendo `clipboard.history` e `clipboard.status`, posto em `C.client` e destruído no fim, os ajudantes `ok`/`ids`/`methods` como os do `tests/lucernaclient.test.js`):
  - `clipboard: tópicos conforme o pedido`: `wantedTopics(false)` / `(true)`.
  - `clipboard: parâmetros do configure e da lista`: `configureParams(50, false, "")` → `{limit: 50, persist: false}`; com `"wayland-1"` → mais `waylandDisplay`; `listParams("all", "  ", 0, 50)` → `{offset: 0, limit: 50}`; `listParams("files", "pdf", 50, 50)` → `{kind: "files", query: "pdf", offset: 50, limit: 50}`.
  - `clipboard: configurar vem antes de assinar`: pelo caminho real do aperto de mão — um cliente novo de `t.lucernaClient()` em `C.client`, `C.watching = true`, `c.opened()` e a resposta do `lucerna.hello` por `c.receive(ok(id, hello(...)))` (o `greeted()` de verdade liga o `connected`, que chega ao `connectionChanged` pelo `Connections`) → entre os pendentes, o `clipboard.configure` tem id menor que o `lucerna.subscribe` de `clipboard.history`.
  - `clipboard: a página pelo fio`: `wired()`, `load("all", "")` → um `clipboard.list` pendente; `c.receive(ok(id, {items: [3 itens], total: 120}))` → `items.length` 3 e `total` 120; `more()` → `clipboard.list` com `offset` 3; a resposta soma 3 + 2, e um item repetido na página nova não entra de novo; uma resposta de um `fetch` anterior (token velho) não muda nada; com uma troca em voo (`loading`), `more()` não pede nada e a resposta da troca vale.
  - `clipboard: a revisão pelo sinal real repagina`: `C.client.received("clipboard.history", JSON.parse(JSON.stringify({revision: 4, count: 9, pinned: 2})))` → `count` 9, `pinned` 2, um `clipboard.list` novo pendente com `limit` = `Math.max(50, items.length)`; a mesma revisão de novo → nenhum pendente a mais.
  - `clipboard: estado do módulo e queda`: `applyStatus({available: true, error: null})` → `moduleAvailable`; `connectionChanged(false)` → falso, `items` `[]`.
  - `clipboard: os métodos saem com os parâmetros`: com `wired()`, `copy(7)`, `remove(7)`, `pin(7, true)`, `clear(false)`, `copyImage("/tmp/a.png")` → os cinco métodos pendentes com `{id: 7}`, `{id: 7}`, `{id: 7, pinned: true}`, `{all: false}`, `{path: "/tmp/a.png"}`.
  - `clipboard: null e tópico desconhecido não mudam nada`; `clipboard: estado original restaurado`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** (`git rm services/scripts/clipboard-watch.sh`). O `ClipboardState` e as configurações ainda leem `entries`: nesta tarefa, eles passam a ler `Clipboard.items`/`count`/`pinned` só o bastante para o `qmllint` e o `unit` ficarem verdes; sai o `Clipboard.start()` do `ClipboardState` (l.141), e os `Binding` passam à ordem `limit`, `persist`, `watching` (o `persist` já certo quando a assinatura sai); o resto é da Tarefa 9.
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE clipboard casos=9`; `grep -rn "wl-copy\|wl-paste\|clipboard-watch\|decode(" services features tests` → nada.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: Clipboard lê do lucernad; saem o wl-paste, o wl-copy e o script"`

---

### Tarefa 9: `lucerna-shell` — o painel com páginas, "Arquivos", o trecho marcado e as configurações

**Files:**
- Modify: `features/clipboard/state/ClipboardState.qml`, `features/clipboard/ui/Clipboard.qml:88-107,140-205`, `features/clipboard/ui/ClipboardItem.qml`, `features/settings/state/ClipboardSettings.qml`, `features/settings/ui/ClipboardPage.qml:18`, `tests/runner.qml` (a suíte nova e o getter `get ClipboardState()` ao lado do `get Clipboard()`), `README.md:104,151` e onde fala do `wl-clipboard`, `Lucerna — Pendências.md:36` (vira `[x]` com "(feito em 2026-10-21: módulo `clipboard` do `lucernad`)")
- Test: `tests/clipboardstate.test.js` (novo)

**Interfaces:**
- Consumes: o `Clipboard` da Tarefa 8.
- Produces:
  - `ClipboardState`: `filters` ganha `{ label: "Arquivos", value: "files" }` depois de "Imagens"; `items` = `Clipboard.items`; `count` = `Clipboard.count`; `available` = `Clipboard.available`; `unavailableText` (`"Indisponível: sem conexão com o lucernad."` sem conexão; `` `Indisponível: ${Clipboard.error}` `` com erro; `"Indisponível."`); `query`/`filter` mudando → um `Timer` de 150 ms chama `Clipboard.load(filter, query)`; abrir o painel chama `Clipboard.load("all", "")`; `function more(): void` (a lista no fim); `move(step)` circular só com tudo carregado: no fim da lista com `items.length < total`, chama `more()` e fica no último; `function marked(text: string, matches: var): string` (o texto escapado para `StyledText` — `&`, `<`, `>` — com cada intervalo de `Array.from(matches)` entre `<b>` e `</b>`; intervalos fora do texto ignorados); `preview(entry)`: imagem → "Imagem", arquivos → `entry.preview`, texto → `entry.preview`; `detail(entry)`: texto "há X · N caracteres · N linhas" (de `size` e `lines`), imagem "há X · L×A", arquivos "há X · N arquivos"; `activate(entry)` → `Clipboard.copy(entry.id)`, fecha e, com `Config.clipboardPaste`, `Clipboard.paste()`; `remove`/`togglePin` → `Clipboard.remove(id)` / `Clipboard.pin(id, !entry.pinned)`.
  - O painel: `ListView` com `onAtYEndChanged: if (atYEnd) ClipboardState.more()`; sem `available`, um `EmptyState` do `core` com `icon: "content_paste_off"` e `text: ClipboardState.unavailableText` no lugar da coluna de hoje (as outras três frases ficam); o `ClipboardItem` mostra `entry.thumb` (`sourceSize` 272 × 168 como hoje) e, sem `thumb`, o ícone `image`; arquivos com o ícone `folder` e uma linha; o texto da prévia com `textFormat: Text.StyledText` e `text: ClipboardState.marked(ClipboardState.preview(entry), entry.matches)`; o botão "Limpar" habilitado por `count > pinned` (do `Clipboard`).
  - `ClipboardSettings`: `count` = `Clipboard.count`, `pinned` = `Clipboard.pinned`; a descrição de "Guardar o que for copiado" sem o `wl-clipboard`: disponível "Textos, imagens e arquivos, num painel próprio (Super+V). Senhas marcadas pelos gerenciadores ficam de fora"; senão o mesmo texto do `unavailableText` (a regra repetida no `ClipboardSettings`, porque uma feature não importa outra).

- [ ] **Passo 1: os testes** (`tests/clipboardstate.test.js`; guarda e restaura `query`, `filter` e o que mexer no `Clipboard`).
  - `clipboardstate: o filtro Arquivos`: `filters.map(f => f.value)` = `["all", "text", "image", "files"]`.
  - `clipboardstate: o trecho marcado`: `marked(text, matches)` com `marked("Uma Ação", [[4, 8]])` = `"Uma <b>Ação</b>"`; `marked("a<b>&", [])` = `"a&lt;b&gt;&amp;"`; `marked("ab", [[1, 9]])` = `"a<b>b</b>"`.
  - `clipboardstate: o trecho marcado pelo modelData`: o caminho de verdade (os `matches` e as `uris` chegam pela resposta do `call` e pelo `modelData` do `ListView`, não pelo `received`) — um cliente de `t.lucernaClient()` com o hello respondido em `Clipboard.client`, `Clipboard.load("all", "acao")` e a resposta por `c.receive(ok(id, {items: [{id: 1, kind: "text", preview: "😀 Ação", matches: [[3, 7]], size: 6, lines: 1, created: 0, used: 0, pinned: false}, {id: 2, kind: "files", preview: "a, b", uris: ["file:///a", "file:///b"], size: 20, created: 0, used: 0, pinned: false}], total: 2}))`; um `Instantiator` criado no teste por `Qt.createQmlObject` (pai: `t.object()`), com `model: Clipboard.items` e o delegate `QtObject { required property var modelData }`; no delegate 0, `Array.from(modelData.matches).map(p => Array.from(p))` = `[[3, 7]]` e `marked(preview(modelData), modelData.matches)` = `"😀 <b>Ação</b>"`; no 1, `detail(modelData)` termina em "2 arquivos". O `Instantiator` e o cliente destruídos e o `Clipboard.client` restaurado no fim.
  - `clipboardstate: detalhe por tipo`: texto `{size: 12, lines: 2}` termina em "12 caracteres · 2 linhas"; imagem `{width: 10, height: 5}` em "10×5"; arquivos com 2 `uris` em "2 arquivos".
  - `clipboardstate: no fim da lista, pede mais`: com `Clipboard.items` de 3 e `total` 10 e `selected` 2, `move(1)` → `selected` 2 e um `clipboard.list` com `offset` 3 pendente; com `total` 3, `move(1)` → `selected` 0.
  - `clipboardstate: indisponível diz por quê`: com `Clipboard.error = "Sem WAYLAND_DISPLAY: …"` e conectado falso → `"Indisponível: sem conexão com o lucernad."`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces e a documentação (o README sem o `wl-clipboard` nas dependências, com o `lucernad` dando também a área de transferência; a Pendência).
- [ ] **Passo 4: verificar.**
  - `dev/ci.sh lint rules unit` → verde, `SUITE clipboardstate casos=6`; `grep -rni "wl-clipboard\|wl-paste\|wl-copy" README.md features services` → nada.
  - Conferência visual com o binário da Tarefa 6 (sem a área de transferência do host): `dev/test.sh up`; os dois `docker cp` e o reinício do `lucernad` do passo 7 da Tarefa 7; a conferência do `WAYLAND_DISPLAY` do `lucernad` pelo `/proc` (Global Constraints); `dev/test.sh run 'printf "Ação de teste" | lucernad-selecao oferecer "text/plain;charset=utf-8" &'` (sempre pelo `run`), o mesmo com `image/png` de uma captura do `grim` e com `text/uri-list`; `dev/test.sh see clip clipboard open`; `dev/test.sh ipc clipboard toggle`; com a busca `acao` digitada por `wtype` (já na imagem), outra captura. Critério: os três itens com a miniatura e o ícone de arquivos, o trecho marcado em negrito, o filtro "Arquivos" na barra; `dev/test.sh errs` sem avisos novos.
- [ ] **Passo 5: commit.** `git commit -m "Área de transferência: o painel em páginas do lucernad, com Arquivos e o trecho marcado"`

---

### Tarefa 10: `lucerna-shell` — o `lucernad` novo no container e na fumaça, PR e merge

**Files:**
- Modify: `dev/lucernad.rev` (`<LUCERNAD_REV>`), `dev/lucernad.sh` (também `cargo build --release --locked -p lucernad-clipboard --example selecao`, instalado como `<prefixo>/bin/lucernad-selecao`), `dev/Dockerfile` (sai o `wl-clipboard` da l.24; `COPY` do `lucernad-selecao` ao lado do `lucernad`), `dev/test.sh:89` (o roteiro copia com `dev/test.sh run '… | lucernad-selecao oferecer …'`, com a conferência do `/proc/<pid>/environ` das Global Constraints antes), `ci/smoke.sh`, `ci/tolerated.txt`

**Interfaces:**
- Consumes: `<LUCERNAD_REV>` (Tarefa 7); `lucernad --json status`, `lucernad --json historico`, `lucernad copiar`, `lucernad-selecao`.
- Produces, em `ci/smoke.sh`:
  - O `lucernad serve` passa a subir com `env -u WAYLAND_DISPLAY` (o `WAYLAND_DISPLAY` chega pelo `configure` do shell: o caminho do spec para o ambiente sem ele).
  - A fase "área de transferência", antes do `step clipboard clipboard open` de hoje: `printf 'olá da fumaça' | setsid lucernad-selecao oferecer 'text/plain;charset=utf-8' text/plain &`; até 5 s, `lucernad --json historico` com 1 item (`row "área de transferência" "texto observado em 5 s" …`); a primeira captura do `grim` copiada com `image/png` → 2 itens com `thumb` existente; `printf segredo | lucernad-selecao oferecer text/plain x-kde-passwordManagerHint` → depois de 1 s, ainda 2 itens (`row … "senha de fora"`); `lucernad --json historico fumaca` → 1 item com `matches`; `lucernad copiar <id do texto>` e `lucernad-selecao ler text/plain` = `olá da fumaça` (`row … "colar de volta"`). O `step clipboard clipboard open` de hoje passa a mostrar os dois itens (a captura `NN-clipboard.png`). Cada conferência reprova com `fail` e o critério; as leituras do JSON por `python3 -c`; os `lucernad-selecao` de fundo encerrados no `cleanup`.
  - O cabeçalho do script ganha a fase "área de transferência".
  - `ci/tolerated.txt`: avisos novos de ambiente só entram medidos, cada bloco com o motivo, linha inteira (`fullmatch`).

- [ ] **Passo 1: a revisão e o container.** Gravar `<LUCERNAD_REV>`; o `lucernad.sh`, o `Dockerfile` e o roteiro do `test.sh`. Verificar: `dev/test.sh up` reconstrói a imagem; `dev/test.sh run 'lucernad --json status'` com `clipboard.status.available` `true` depois de o shell conectar; `dev/test.sh run 'command -v wl-copy'` → nada; o `WAYLAND_DISPLAY` do `lucernad` que o `dev/hyprland.lua` sobe, pelo `/proc/<pid>/environ`, é o do Hyprland aninhado (se for `wayland-host`, o `hyprland.lua` passa a subir o `lucernad` com o `WAYLAND_DISPLAY` do próprio Hyprland, e isso entra no commit); a conferência visual da Tarefa 9 de novo, sem `docker cp`.
- [ ] **Passo 2: a fumaça.** `smoke.sh` e `tolerated.txt`. Verificar: `dev/ci.sh smoke` → verde, com as quatro linhas da "área de transferência" no `ci-out/smoke/summary.md` e a captura do painel com os dois itens; `dev/ci.sh` (os quatro) → verde.
- [ ] **Passo 3: commit, PR e merge.** `git commit -m "Área de transferência: o lucernad novo no container e na fumaça, sem o wl-clipboard"`; `git push -u origin feat/area-de-transferencia`; `gh pr create --repo lucerna-desktop/lucerna-shell --base main --title "Área de transferência: o shell lê do lucernad" --body <resumo; a revisão fixa do lucernad; como foi testado (unidade pelo fio do cliente e pelo sinal real, fumaça com o lucernad-selecao, a conferência da Tarefa 7); as capturas; o Capture.copyImage pelo daemon>`. **Controlador:** revisão final da branch (Opus), CI verde, `gh pr merge <n> --repo lucerna-desktop/lucerna-shell --merge`.

---

## Autorrevisão

**Cobertura do spec:** observação pelo `ext-data-control-v1` com o `wlr` de reserva, sem processo externo (Tarefa 4); exibição do ambiente ou do `configure`, com erro claro (Tarefas 1 e 5); conexão só com assinante ou com a seleção nossa, segurada pelo `WorkTracker` (Tarefa 5); escolha do tipo na ordem do spec, limites, prazos e nada de HTML (Tarefa 1); sensível sem ler (Tarefas 1, 5 e 7); cópias nossas só sobem (Tarefas 1 e 5); SQLite em WAL no `$XDG_DATA_HOME/lucerna/clipboard`, item com os campos do spec, imagens e miniatura de 272 px, deduplicação, limite só dos não fixados com os arquivos, sem persistência na memória e no `$XDG_RUNTIME_DIR`, a troca nos dois sentidos, órfãos (Tarefas 2–3); só depois do `configure` ou 2 s (Tarefa 5); busca trigram e comparação curta sem acentos, com trechos, e lista paginada com fixados primeiro (Tarefas 2–3); cópia de volta com os tipos do spec, segurada se o shell recarregar (Tarefas 1, 4, 5 e 7); colar sozinho no shell (Tarefa 8); os tópicos, os métodos, a faixa 1–1000 e o `-32602` (Formato, Tarefa 5); o adaptador, o `configure` ao conectar e ao mudar, a página, o filtro "Arquivos", o trecho, o "Indisponível" (Tarefas 8–9); o que sai do shell (Tarefas 8–10); a fumaça sem `wl-clipboard` (Tarefa 10); sem migração (nada lê o `clipboard.json`); os testes Rust com dublê, a sonda real nos dois protocolos, a máquina real e o CI dos dois repositórios (Tarefas 1–7 e 10). Sem lacunas.

**Passos:** cada passo de código traz as assinaturas, os textos e os números; os únicos algoritmos escritos são o laço e a ordem das cópias, que as assinaturas não determinam. Os testes ficam como nome e asserções-chave.

**Tipos:** `Kind`, `Choice`, `choose`, `offers_for`, `read_pipe`, `ReadError`, `socket_path` (Tarefa 1); `normalize`, `find_matches`, `preview_for`, `file_names`, `Thumb`, `thumbnail`, `extension_for` (Tarefa 2); `Store`, `NewClip`, `Origin::{Disk, Memory { files, disk }}`, `Prepared`, `prepare`, `wipe`, `ListQuery`, `Page` (Tarefa 3; `prepare` e `insert` usados pela Tarefa 5); `sniff` (Tarefa 2, usado pelo `copyImage` da 5); `testing::isolated` (Tarefa 4); `Event`, `Source`, `Control::{receive, offer}`, `Session`, `Backend::open(&Path)`, `Wayland { timeout, only }`, `FakeBackend::{copy, copy_stuck, cancel, disconnect, paste, owned, sessions, opened, sockets, reads}` (Tarefas 4–5); `ClipboardConfig`, `register(…, WorkTracker)`, `quiet_config(&Path)` (Tarefas 5–6) batem entre si; no shell, `load`, `more`, `fetch`, `applyPage`, `loading`, `items`, `total`, `count`, `pinned`, `error` e `marked(text, matches)` batem entre as Tarefas 8 e 9.

**Review Focus:** os cinco itens têm teste na tarefa dona (1, 2, 3, 5 e 9), e o item 3 também a conferência da Tarefa 7.

**Proporção:** o plano é maior que o spec porque cobre dois repositórios, a sonda, a fumaça e a conferência; o código fica nas assinaturas, e os blocos de código são só o laço, o formato e os comandos que precisam ser exatos.
