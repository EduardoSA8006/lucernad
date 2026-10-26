# lucernad: o módulo de rede externa (letras e clima) — plano de implementação

> **Para agentes:** SUB-SKILL OBRIGATÓRIO: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para implementar este plano tarefa por tarefa. Os passos usam checkbox (`- [ ]`) para acompanhamento.

**Objetivo:** o módulo `web` do `lucernad` (letras da LRCLIB e do Vagalume com casamento confiável, previsão e busca de cidade do Open-Meteo, cliente HTTP próprio com prazo e nova tentativa, cache em disco, modo offline e a conectividade do NetworkManager) e o shell lendo dele, com o `XMLHttpRequest` fora e a aba Clima redesenhada no desenho do Caelestia.

**Arquitetura:** dois repositórios. `lucernad`: o crate novo `lucernad-web` (o LRC e o casamento como funções puras, o HTTP atrás do trait `Http` com a nova tentativa por cima, o cache em disco genérico, as fontes como funções sobre o `Http`, a conectividade atrás do trait `Connectivity`, o laço guiado pelo assinante) e a feature `web` (padrão) no binário, com os subcomandos `faixa`, `cidade` e `rede` e o `LUCERNAD_WEB_BASE` (só de teste: tudo para um servidor local). `lucerna-shell`: o `services/Web.qml` novo (estado e configuração do módulo), `Lyrics.qml` e `Weather.qml` viram adaptadores, a aba Clima é reescrita e a fumaça usa um servidor falso local (`ci/web-fake.py`). O `lucerna-sdk` não muda.

**Tecnologia:** Rust 1.95 (edição 2024); `reqwest` 0.13 (0.13.5, `rustls-no-provider` + `query`, sem as features padrão) com `rustls` 0.23 só com o `ring` (sem OpenSSL, sem `aws-lc`/`cmake`; certificados do sistema pelo `rustls-platform-verifier`; o crate compila com `unsafe_code = "deny"`, conferido na sonda); `zbus` 5 (já no workspace, para o NetworkManager); `sha2`, `unicode-normalization`, `lucerna-paths` (já no workspace); `tokio`, `serde_json`, `lucernad-core`, `lucerna-service` do SDK; QML do Quickshell 0.3.2; Hyprland 0.56.2 (container do `dev/test.sh`), sway 1.12 headless (fumaça); `dbus-daemon` só no container da sonda; GitHub Actions; `gh`.

**Spec:** `docs/superpowers/specs/2026-10-24-rede-externa-design.md` (neste repositório, PR #15; leia inteiro antes da primeira tarefa). Base: os planos da energia (`docs/superpowers/plans/2026-10-22-energia-e-brilho.md`) e da área de transferência, cujos padrões este plano segue.

## Global Constraints

- **Ordem de execução:** Tarefas 1–7 (`lucernad`, branch `feat/web`) → Tarefas 9, 10 e 11a (`lucerna-shell`, branch `feat/rede-externa`; só dependem do Formato e podem andar em paralelo com 1–7) → Tarefa 11 (a aba Clima, conferida no container com o binário da `feat/web`: depois da 7) → Tarefa 8 (conferência na máquina real, precisa do binário da 7 e do shell da 11; PR e merge do `lucernad`) → Tarefa 12 (precisa do `<LUCERNAD_REV>`; fumaça, PR e merge do shell).
- **Branches:** no `lucernad`, `git fetch` e confirme que o `origin/main` já tem o spec (o PR #15, com auto-merge); a `feat/web` parte dele, e o primeiro commit dela é este plano (feito pelo controlador). No `lucerna-shell`, `git switch -c feat/rede-externa origin/main` no checkout principal (o `dev/test.sh` monta esse checkout).
- **Merge:** cada PR é aberto pela tarefa que fecha o repositório; o merge é do **controlador**, por merge commit (`gh pr merge <n> --merge`), depois da revisão final da branch (Opus) e do CI verde. Os rulesets exigem `fmt`/`clippy`/`test` (`lucernad`) e `lint`/`rules`/`unit`/`smoke` (shell).
- Contas e remotes: `gh` com a conta `EduardoSA8006`; o `origin` é `github-pessoal:lucerna-desktop/<repo>.git`.
- Toolchain: `cargo`/`rustc` em `~/develop/cargo/bin` (ponha no `PATH`); `rust-toolchain.toml` com `channel = "1.95"`. O SDK continua em `rev = "f55ba66dc18b62fab075b20691ad9edb7ca22a1c"`.
- Dependências novas só em `[workspace.dependencies]`, exatamente: `reqwest = { version = "0.13", default-features = false, features = ["rustls-no-provider", "query"] }`, `rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12"] }`; o `tokio` do crate ganha `io-util` (no `Cargo.toml` dele: `tokio = { workspace = true, features = ["io-util"] }`, para o `LocalServer`), `lucernad-web = { path = "crates/lucernad-web" }`. `unsafe_code = "deny"` vale para tudo. **Sem dependência de sistema nova** (nada de OpenSSL nem `cmake`): o CI não ganha `apt-get`.
- **Nenhum processo externo** em `crates/*/src` (nada de `curl`, `nmcli`, `busctl`).
- **Nenhum teste automatizado sai para a internet:**
  - os testes Rust usam só o `FakeHttp` ou o `ReqwestHttp` contra o `testing::LocalServer` (127.0.0.1); `WebConfig::system()`, `NmConnectivity::system()` e `Endpoints::real()` só aparecem no `Modules::real()`, no `WebConfig::system()` e nos testes de `endpoints`/`sonda`. Verificação de toda tarefa Rust: `grep -rnE "WebConfig::system|NmConnectivity::system|Endpoints::real" crates/*/tests crates/*/src/testing.rs` → só `crates/lucernad-web/tests/{endpoints,sonda}.rs`;
  - os testes de processo (`crates/lucernad/tests/process.rs`) rodam todo binário com `LUCERNAD_WEB_BASE` = um `LocalServer` (o `env_clear` de hoje deixaria o binário ir às fontes de verdade);
  - a prova (Tarefa 1, de novo na 7 e na 8): a suíte inteira passa sem rede, `U=$(id -u) G=$(id -g); unshare -rn sh -c "ip link set lo up && exec unshare --map-user=$U --map-group=$G env TMPDIR=/tmp cargo test --workspace --locked --offline"`. O `unshare` de dentro volta ao uid/gid do usuário: só o `-rn` rodaria como root mapeado e quebraria os testes de permissão (`crates/lucernad-clipboard/tests/store.rs:615-630`). Confirmada na sonda (g) na `main` de hoje;
  - a fumaça do shell sobe o `lucernad` com `LUCERNAD_WEB_BASE` apontando para o `ci/web-fake.py`.
- **A chave do Vagalume nunca aparece** em log, tópico, mensagem de erro, `Debug` de pedido ou linha de comando (o `rede` a lê do ambiente, `LUCERNAD_VAGALUME_KEY`, nunca de argumento); os erros do HTTP nunca carregam a URL (a query tem a chave).
- **Limites de recurso:** corpo de resposta até `BODY_MAX` (4 MiB, lido em pedaços; o resto não é lido); letra com até `LINES_MAX` (1000) linhas de até `LINE_CHARS_MAX` (200) caracteres e até `TEXT_BYTES_MAX` (128 KiB) de texto somado em UTF-8 (as linhas além disso caem); entrada do cache até `ENTRY_MAX` (512 KiB); o cache inteiro até `CACHE_LIMIT` (50 MiB); até 5 cidades; nenhuma mensagem do tópico passa de 1 MiB (o `MAX_MESSAGE` do SDK): o pior caso da letra é 128 KiB de texto, no máximo dobrado pelo escape do JSON (sem caracteres de controle, que saem), mais ~40 bytes por linha × 1000, abaixo de 300 KiB.
- Tópicos, métodos, textos de erro e limites exatamente como em "Formato".
- Textos em português do Brasil, com acentos (mensagens, comentários, documentação, commits no estilo `Rede: o LRC e o casamento de faixas`); nomes de código em inglês. Texto vindo de fora (nome de cidade, de faixa, letra) passa pelo `plain()` antes de ir ao terminal.
- Regras do shell: `qmllint` com 0 avisos; 100% das funções do escopo de `tests/coverage.json` chamadas pelos testes; camadas do `ci/rules.py`; funções declaradas como `function nome(...) {` numa linha; um teste que muda um singleton restaura o estado no fim; **todo dado que chega do daemon passa por `Array.from` antes de `map`/`filter`/`find`**, e os testes mandam os dados pelo caminho real (`client.received(tópico, JSON.parse(JSON.stringify(…)))`, a resposta pelo `client.receive` do `wired()`); **disponível = `client.connected && error === ""`**.
- Verificação de toda tarefa Rust, além do teste dela: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo clippy -p lucernad --no-default-features --all-targets --locked -- -D warnings`, o mesmo com `--features system`, `spectrum`, `clipboard`, `power` e (da Tarefa 7 em diante) `web`, e `TMPDIR=/tmp cargo test --workspace --locked` limpos. O `Cargo.lock` atualizado vai no commit da tarefa que põe a dependência. Toda tarefa do shell: `dev/ci.sh lint rules unit` (e `smoke` na Tarefa 12) verdes.

## Review Focus

1. **A chave do Vagalume vazando por um erro** (o `reqwest::Error` mostra a URL com a `apikey`; um `{:?}` do pedido num `assert` ou num `eprintln!`): nenhum texto publicado, logado ou devolvido contém a chave nem a URL. Testes: `errors_never_carry_the_url` (Tarefa 2), `network_error_is_an_error_not_none` (Tarefa 4), `vagalume_key_never_leaks` (Tarefa 6).
2. **Pular faixas em sequência** (dez `web.track` em 1 s com a aba Mídia aberta e a rede lenta): no máximo uma busca em voo, nenhuma letra de faixa anterior publicada depois da troca, e o estado final é o da última faixa. Teste: `skipping_tracks_never_shows_old_lyrics` (Tarefa 6).
3. **Portal cativo, resposta enorme ou quebrada** (HTML com 200, corpo de 50 MB, JSON truncado): vira `error` (nunca "sem letra" guardado por 7 dias), sem ler além de 4 MiB. Testes: `body_over_the_limit_is_refused` (Tarefa 2), `captive_portal_html_is_an_error` (Tarefa 4), `none_is_cached_and_failure_is_not` (Tarefa 6).
4. **Modo offline ligado com um pedido em voo, ou antes do primeiro `configure`** (o daemon sobe antes de o shell dizer que está offline): nada sai antes do `configure`, e depois do `offline: true` nenhum pedido novo sai e a resposta em voo não é publicada. Testes: `nothing_goes_out_before_configure` e `going_offline_mid_fetch_sends_nothing_more` (Tarefa 6).
5. **Trocar de cidade com a previsão da anterior em voo**: a previsão velha nunca aparece sob o nome novo, nem vai para o cache da cidade nova. Teste: `changing_city_mid_fetch_publishes_only_the_new_one` (Tarefa 6).

---

## Sondas feitas antes do plano (2026-10-24, só leitura, sem nenhum pedido às APIs)

| Sonda | O que se viu |
| --- | --- |
| (a) crates | Num projeto descartável com o `rust-toolchain.toml` e `unsafe_code = "deny"`, só `cargo check`, sem nenhum pedido: `reqwest` 0.13.5 com as features padrão puxa o `aws-lc-rs` (compila aqui porque há `cmake`; numa máquina sem ele, falha); com `rustls-no-provider` + `query` e `rustls` 0.23.46 com `ring`, compila sem `aws-lc` nem `cmake` (118 crates, 8 s), com os certificados do sistema (`rustls-platform-verifier` 0.7.1); o provedor é instalado na execução (`rustls::crypto::ring::default_provider().install_default()`). Na 0.13, `rustls-tls` não existe mais e o `query` é feature à parte. `reqwest` 0.12.28 (`rustls-tls`, `ring`, `webpki-roots`) e `ureq` 3.5.0 (30 crates, síncrono: precisaria de `spawn_blocking`) também compilam. Nenhuma combinação pede OpenSSL. O `unicode-normalization` (`nfd` + `is_combining_mark`) dá `Acao` para `Ação`. Servidor de teste: `TcpListener` cru, `hyper` 1.12, `wiremock` 0.6.5 e `httpmock` 0.9.0 compilam. |
| (b) NetworkManager | `org.freedesktop.NetworkManager` em `/org/freedesktop/NetworkManager`: propriedade `Connectivity` (`u`, `emits-change`; valor agora 4), `State` (`u`, 70), sinal `StateChanged(u)` (o `State`, não a conectividade), `CheckConnectivity() → u`, `ConnectivityCheckEnabled` true. A conectividade muda pelo `PropertiesChanged` do `org.freedesktop.DBus.Properties`. Valores do NM: 0 desconhecida, 1 nenhuma, 2 portal, 3 limitada, 4 completa. |
| (c) Vagalume | A doc oficial (`api.vagalume.com.br/docs/`, `/docs/letras/`, `/terms/`) respondeu 503 nas duas sondas. Por clientes de terceiros: `https://api.vagalume.com.br/search.php` com `art`, `mus`, `apikey` (`extra`, `nolyrics`, `musid` opcionais); resposta `{type, art: {id, name, url}, mus: [{id, name, url, lang, text, translate}]}`; `type` `notfound`/`song_notfound` confirmados, `exact`/`aprox` não; sem duração; a chave se cria em `https://auth.vagalume.com.br/settings/api/`; um aviso antigo pedia o logo e o link da página da letra. **A Tarefa 4 relê a doc e os termos antes de implementar** (passo 0). |
| (d) LRCLIB e Open-Meteo | LRCLIB (código do servidor, `tranxuanthang/lrclib`): `/api/get` com `track_name` e `artist_name` obrigatórios, `album_name` e `duration` (f64, 1–3600) opcionais, a duração casada em ±2 s; 404 `{"statusCode":404,"name":"TrackNotFound","message":…}`; 503 `ServerOverloaded` com `Retry-After`; `/api/search` com `q`/`track_name`/`artist_name`/`album_name` (sem `q` nem `track_name` → `[]`), até 20 resultados; campos `id, trackName, artistName, albumName, duration (f64), instrumental, plainLyrics, syncedLyrics`; sem `get-cached`; nenhum texto de `User-Agent` exigido no código (lê `Lrclib-Client`, `X-User-Agent` ou `User-Agent`). Open-Meteo: `daily` com `sunrise`/`sunset` em ISO local sem fuso (`2026-10-24T05:41`) com `timezone=auto`; `forecast_days` 0–16; geocoding `name`, `count` (máx. 100), `language`, `format`; resultados `name, latitude, longitude, country, country_code, admin1…` (campos vazios omitidos; sem resultado a doc não diz se `results` some); erro 400 `{error, reason}`; dados CC BY 4.0 com link obrigatório ("Weather data by Open-Meteo.com"); uso livre só não comercial, < 10 000 chamadas/dia. |
| (e) segredos no shell | Não há lugar de segredo (nada de keyring/libsecret no shell); as preferências ficam no `config.json` do `Quickshell.statePath` (`~/.local/state/quickshell/by-shell/<id>/`, com o `~/.local/state` em 0700 na máquina). O cliente (`LucernaClient.call`) não loga parâmetros. |
| (f) testes sem internet | Rust: um servidor HTTP/1.1 mínimo sobre `tokio::net::TcpListener` em 127.0.0.1:0 (só `tokio`, sem dependência nova; `wiremock`/`httpmock` compilam mas não fazem falta) e o dublê `FakeHttp` para a lógica; `unshare -rn` sem root funciona (prova da Tarefa 7). Fumaça: o job tem `python` (`ci/packages/smoke.txt`), o `mpv` traz o `ffmpeg` (dependência no Arch); o player da fumaça toca ruído sem título nem artista, e o módulo não busca faixa sem artista (Decisões): o `ffmpeg` gera um arquivo com as etiquetas. |

| (g) prova sem rede | Na `main` de hoje (76c2a84), `U=$(id -u) G=$(id -g); unshare -rn sh -c "ip link set lo up && exec unshare --map-user=$U --map-group=$G env TMPDIR=/tmp cargo test --workspace --locked --offline"` → exit 0, 49 binários de teste, 315 passed, 10 ignored (as sondas isoladas), 0 failed. Com só o `unshare -rn` (root mapeado), o teste de permissão do clipboard falha. |

## Decisões deste plano (o que o spec deixava ao construtor)

| Tema | Decisão | Por quê |
| --- | --- | --- |
| Crate | `crates/lucernad-web` (biblioteca, feature `web` padrão no binário), com `testing` como os outros | o padrão da casa |
| HTTP | `reqwest` 0.13 assíncrono (o daemon já é `tokio`), `rustls` com `ring` instalado como provedor no `ReqwestHttp::new` (um `install_default` que já instalado não é erro), certificados do sistema; um `Http` só faz uma tentativa; a nova tentativa (`fetch`) fica por cima; nas fontes de verdade, `https_only(true)` e redirecionamento só para o mesmo host, até 3 (`same_host_redirect`); com o `LUCERNAD_WEB_BASE`, `redirect::Policy::none()`; `connect_timeout` 5 s e prazo total `REQUEST_TIMEOUT` (10 s); o corpo lido em pedaços até `BODY_MAX` | sonda (a); a política testável sem servidor |
| User-Agent | `lucernad/<versão> (Lucerna; https://github.com/lucerna-desktop/lucernad)` | o spec; a LRCLIB lê o `User-Agent` (sonda d) |
| Nova tentativa curta | o `web.searchCity` usa `RetryPolicy::short()` (1 nova tentativa, 1 s, `Retry-After` até 5 s): pior caso 10 + 1 + 10 s, dentro do `CALL_TIMEOUT` (30 s) da linha de comando do SDK | a busca é interativa |
| Nova tentativa | erros passageiros = prazo, conexão, 429 e 5xx; até 3 novas tentativas (4 pedidos) com espera de 1, 2 e 4 s; com `Retry-After` em segundos (429 e 503), espera o pedido, até 60 s (mais que isso → desiste com `WaitTooLong`); data HTTP no `Retry-After` → a espera padrão; 4xx (fora 429) volta como resposta, sem nova tentativa | o spec; o 503 da LRCLIB traz `Retry-After` |
| Antes do `configure` | o módulo fica **offline** até o primeiro `web.configure` (lê o cache, não vai à rede) | o anti-objetivo "tentar a rede com o modo offline ligado": o daemon sobe antes do shell |
| `configure` | substitui tudo: campo opcional ausente = `null`/padrão (`refresh` 30); quem manda é um dono só no shell (`services/Web.qml`) | dois adaptadores mandando metades se atropelariam |
| Conectividade | `Connectivity` 1 (nenhuma) → sem rede; 0, 2, 3, 4 → tenta; sem NetworkManager (sem barramento, nome ausente) → tenta; acompanha o `PropertiesChanged` do `Connectivity` e o `NameOwnerChanged`; ao voltar, refaz a letra com erro e o clima velho | o spec ("não tenta quando é `none`") |
| Faixa sem artista ou sem título | `{state: "none"}` sem nenhum pedido | a LRCLIB exige os dois; na dúvida, nenhuma letra |
| Sufixos ignorados | entre parênteses ou colchetes, ou depois do último ` - `, quando o trecho tem (normalizado) `remaster`, `remasterizad`, `ao vivo`, `live`, `version`, `versao`, `acustic`, `acoustic`, `radio edit`, `edit`, `mono`, `stereo`, `deluxe`, `bonus`, `demo`, `explicit`, `feat`, `ft`, `part`, `com`; e `feat. X`/`ft. X`/`part. X` no fim do título; **não** `instrumental` nem `parte N` | mudam só a gravação, não a letra; `instrumental` muda a letra |
| Artista | casa se os nomes inteiros normalizados forem iguais **ou** o primeiro artista dos dois (separadores `,`, `&`, ` feat. `, ` ft. `, ` part. `, ` x `) | o Spotify manda "A, B"; a fonte, só "A" |
| Normalização | NFD sem marcas combinantes, minúsculas, tudo que não é letra nem número vira espaço, espaços juntados | o spec |
| Letra | `time` em segundos (≥ 0) na sincronizada, `null` na de texto; linhas de controle removidas; LRC com `[mm:ss.xx]` (minutos de 1 a 3 dígitos, fração de 0 a 3 dígitos), várias marcas por linha, `[offset:±ms]` (positivo adianta: `t − offset/1000`, limitado a 0), etiquetas (`[ar:]`, `[ti:]`, `[length:]`…) ignoradas, linha marcada sem texto = pausa (`""`); ordem estável por tempo | o spec; o shell já usa −1 para "sem tempo" (o adaptador converte) |
| Vagalume | só com a chave; `type` `exact` ou `aprox` com `mus[0]` que case (título e artista, sem duração) e `text` não vazio; `notfound`/`song_notfound` → nenhuma; outro `type` → erro de formato; `url`: absoluta em `https://www.vagalume.com.br/` vale, começando por `/` ganha o prefixo, qualquer outra vira `https://www.vagalume.com.br/` | a sonda (c) não confirmou `exact`/`aprox`: a Tarefa 4 confere na doc; o link nunca leva para fora do Vagalume |
| Cache | `Cache` genérico de JSON em `$XDG_CACHE_HOME/lucerna/web/{lyrics,weather}/<sha256 da chave>.json` (`AppPaths::new("web", …).cache_dir()`, pasta 0700 pelo `create_private_dir`), `{savedAt, value}`, escrita atômica (`.tmp-*` + `rename`), leitura até `ENTRY_MAX + 1`; arquivo estragado é apagado; depois de cada escrita, apaga os mais antigos (por `mtime`) até caber em `CACHE_LIMIT`; sem pasta (sem `HOME`) o módulo roda sem cache, com um aviso no log | o spec deixa o formato; arquivo por chave dispensa banco |
| Chave do cache | letra: `Track::key()` = artista normalizado + `\u{1f}` + título normalizado + `\u{1f}` + duração arredondada em segundos (ou `-`); clima: `Location::place_key()` = `"{lat:.2},{lon:.2}"` | o spec ("artista + título + duração arredondada") |
| "Sem letra" | guardada 7 dias com `vagalume: bool` (se o Vagalume foi consultado); com a chave nova e `vagalume: false`, busca de novo | pôr a chave depois não fica preso a uma semana de "sem letra" |
| Quando busca a letra | só com assinante de `web.lyrics`; o cache responde na hora; a rede espera `TRACK_SETTLE` (500 ms) sem nova troca antes do primeiro pedido; uma troca aborta a busca em voo | o spec; pular faixas não dispara uma busca por faixa |
| Clima | `state`: `"none"` (sem cidade), `"loading"` (buscando sem nada guardado, ou com o guardado velho), `"ready"`, `"error"` (a última busca falhou; os dados guardados continuam); o `Forecast` guarda o `utc_offset_seconds` da resposta e o nascer/pôr de cada dia; **ao publicar** (o novo e o guardado), o `Forecast::view(now)` calcula a data local da cidade (`now + utc_offset`), descarta os dias anteriores a ela e tira `sunrise`/`sunset` do dia com essa data (nenhum → `null`), nunca pelo índice 0: a previsão de ontem mostrada offline não chama ontem de hoje; texto ISO local `AAAA-MM-DDTHH:MM`; datas `AAAA-MM-DD`; nulos na resposta viram `None`; `precipitation` = `precipitation_probability_max` (%); depois de um erro, nova tentativa em `WEATHER_RETRY` (5 min, ou o `refresh` se menor) | o spec; a hora local da cidade sem conversão de fuso no shell |
| `web.refresh` | busca o clima já (com ou sem assinante; com cidade e online) e refaz a letra em `error` (com assinante) | o botão "Atualizar agora" das Configurações não assina o tópico |
| Busca de cidade | `{results: [{name, state, country, countryCode, latitude, longitude}]}` (até 5; texto ausente = `""`, cortado em 100 caracteres); sem `results` na resposta → `[]` | o spec; a sonda (d) |
| Base de teste | `LUCERNAD_WEB_BASE=http://127.0.0.1:<porta>` (ou `http://[::1]:<porta>`; só IPs literais de loopback, sem `localhost`, que depende do resolvedor): as quatro fontes viram `<base>/lrclib`, `<base>/vagalume`, `<base>/forecast`, `<base>/geocoding`; vazia = ausente; qualquer outra coisa (https, outro host, sem porta, com caminho, com usuário) → o módulo indisponível com o erro do Formato, **nunca** a rede de verdade; com a base, o cache vai para uma subpasta própria (`<cache>/web/teste/`: o guardado de teste nunca aparece no uso real) e não há redirecionamento; o daemon escreve na partida `lucernad: LUCERNAD_WEB_BASE=<base>: a rede é de teste (servidor local, nada vai à internet)`. **Para o PR** | a fumaça e os testes de processo sem internet; só loopback, para a chave nunca ir a outro host |
| Linha de comando | `lucernad faixa ARTISTA TÍTULO [SEGUNDOS] [--album ÁLBUM]` (`web.track`); `lucernad cidade BUSCA` (`web.searchCity`, uma linha `nome, estado, país  (lat, lon)` por cidade pelo `plain()`, "Nenhuma cidade encontrada." sem nenhuma); `lucernad rede [--offline] [--cidade NOME --lat LAT --lon LON] [--atualizar MIN]` (`web.configure`; a chave do Vagalume só pelo `LUCERNAD_VAGALUME_KEY`). **Para o PR** | a conferência e a fumaça sem o shell; a chave fora do histórico do terminal e do `ps` |
| Shell: dono da configuração | `services/Web.qml` (dono `"web"`: `web.status`, o `web.configure` a cada conexão e a cada mudança, o `web.refresh`, o `web.searchCity`); `Lyrics` (dono `"lyrics"`) e `Weather` (dono `"weather"`) só leem os tópicos deles | um `configure` inteiro, de um lugar |
| Shell: a faixa | o `Lyrics` manda `web.track` a cada mudança de título, artista, álbum ou duração arredondada do `Media`, com ou sem a aba aberta; assina `web.lyrics` com `active` (aba Mídia à mostra, letra ligada e fora do modo offline) | o spec ("o shell manda a cada troca; assina só com a aba") |
| Shell: a chave do Vagalume | `Config.vagalumeKey` (no `config.json` do estado, que fica em `~/.local/state`, 0700); campo com `echoMode: TextInput.Password`; só letras, números, `-` e `_`, até 128; nunca em `console.*` | sonda (e): não há lugar de segredo; o spec deixa a configuração |
| Shell: créditos | num lugar só (decisão do usuário, spec atualizado): a página "Créditos" das Configurações e a seção do README (Tarefa 11a); nada nos painéis: a letra do Vagalume sem rótulo (o `Lyrics` guarda `source` e `url`, prontos para o rótulo se o Vagalume pedir) e a aba Clima sem "Open-Meteo"; a tela Sobre sem "Serviços na internet" | o spec |
| Shell: rodapé da aba Clima | fica só o aviso (o `notice` ou o `updatedNote`), discreto, à esquerda, e só quando há o que dizer: offline, sem internet e a hora da previsão velha dizem por que o que se vê pode estar velho; sem crédito | o aviso de offline do rodapé de hoje continua útil no desenho novo |
| Shell: ícones | `Icons.sunrise` = `"wb_twilight"`, `Icons.sunset` = `"bedtime"` (os do Caelestia); o `Icons.forWeather` de hoje para os códigos WMO, sem mudança | o spec deixa ao construtor; a tabela de hoje já cobre todos os códigos |
| Shell: IPC | `qs ipc call weather search NOME`, `weather choose N` (devolve o nome escolhido, `""` sem resultado), `weather refresh` | o "Atualizar agora no IPC" do spec; a fumaça escolhe a cidade sem clique |
| Fumaça | `ci/web-fake.py` (Python sem dependência, `-I`) num porto livre de 127.0.0.1, com respostas fixas: a faixa "Canção da Fumaça" de "Lucerna" com LRC sincronizado, 404 `TrackNotFound` para o resto, `[]` na busca, a previsão de 7 dias e três cidades para "Cacau"; o `mpv` toca um arquivo de ruído com essas etiquetas, gerado pelo `ffmpeg` | sonda (f) |

## Formato

```jsonc
// web.lyrics — tópico comum; null até o primeiro web.track com assinante
{"key":"legiao urbana\u001ftempo perdido\u001f201","state":"found","synced":true,
 "lines":[{"time":12.5,"text":"Todos os dias quando acordo"},{"time":17.0,"text":""}],
 "source":"lrclib"}
{"key":"…","state":"found","synced":false,"lines":[{"time":null,"text":"…"}],"source":"vagalume",
 "url":"https://www.vagalume.com.br/legiao-urbana/tempo-perdido.html"}
{"key":"…","state":"searching","synced":false,"lines":[],"source":""}
{"key":"…","state":"none","synced":false,"lines":[],"source":""}
{"key":"…","state":"error","synced":false,"lines":[],"source":"","error":"Sem conexão com a internet."}
// web.weather — tópico comum; null até o primeiro configure
{"location":{"name":"Cacaulândia, Rondônia, BR","latitude":-10.3375,"longitude":-62.8986},
 "state":"ready","updatedAt":1792847400,
 "current":{"temperature":31.2,"apparent":35.0,"humidity":58,"wind":7.6,"code":2,"isDay":true},
 "daily":[{"date":"2026-10-24","code":95,"min":22.1,"max":33.4,"precipitation":80}],
 "sunrise":"2026-10-24T05:41","sunset":"2026-10-24T18:03"}
{"location":null,"state":"none","updatedAt":null,"current":null,"daily":[],"sunrise":null,"sunset":null}
// com "error": "…" quando state = "error" (os dados guardados continuam)
// web.status — tópico comum
{"available":true,"error":null,"online":true}
```

- `daily` só traz de hoje em diante (a data local da cidade); `sunrise`/`sunset` são os do dia de hoje, ou `null`.
- `url` só existe com `source` `vagalume`; `error` só existe com erro; `updatedAt` em segundos Unix; unidades °C, km/h e %.
- Métodos: `web.track {artist, title, album?, duration?}` → `{}`; `web.configure {offline, vagalumeKey?, location?, refresh?}` → `{}`; `web.searchCity {query}` → `{results: [{name, state, country, countryCode, latitude, longitude}]}`; `web.refresh {}` → `{}`.
- Erros `-32602` (`RpcError::invalid_params`, texto **sem** ponto final e sem ecoar o valor): `"artist precisa ser um texto com até 500 caracteres"`, `"title precisa ser um texto com até 500 caracteres"`, `"album precisa ser um texto com até 500 caracteres, ou null"`, `"duration precisa ser um número entre 0 e 86400 (segundos), ou null"`, `"offline precisa ser verdadeiro ou falso"`, `"vagalumeKey precisa ser um texto de 1 a 128 letras, números, - ou _, ou null"`, `"location precisa ser {name, latitude, longitude} (nome com até 200 caracteres, latitude entre -90 e 90, longitude entre -180 e 180), ou null"`, `"refresh precisa ser um inteiro de 5 a 1440 (minutos), ou null"`, `"query precisa ser um texto de 1 a 100 caracteres"`.
- Erros do app (`web.searchCity`): `1001` "Modo offline ligado: a busca de cidade usa a internet."; `1002` "Sem conexão com a internet."; `1003` "Não consegui falar com o Open-Meteo: <e>."; `1004` "O módulo de rede está indisponível: <error do status>." (também o `web.track` e o `web.configure` com o módulo indisponível).
- `error` da letra: "Modo offline: a letra não é buscada.", "Sem conexão com a internet.", "Não consegui falar com a LRCLIB: <e>.", "A LRCLIB respondeu fora do formato.", "Não consegui falar com o Vagalume: <e>.", "O Vagalume respondeu fora do formato.".
- `error` do clima: "Modo offline: o clima não é atualizado.", "Sem conexão com a internet.", "Não consegui falar com o Open-Meteo: <e>.", "O Open-Meteo respondeu fora do formato.".
- `<e>` (o `HttpError::message`, sem URL): "o servidor demorou demais para responder", "não consegui conectar", "o servidor respondeu <n>", "a resposta passou de 4 MiB", "o servidor pediu para esperar <n> s", "a resposta veio fora do formato" (só na busca de cidade), ou o texto do `reqwest` sem a URL.
- `web.status.error`: "O módulo de rede parou por um erro interno.", "LUCERNAD_WEB_BASE inválido: precisa ser http://127.0.0.1:<porta> ou http://[::1]:<porta> (só para testes).", "Não consegui iniciar o cliente HTTP: <e>.".

## Mapa de arquivos

**`lucernad`:**

```
Cargo.toml  Cargo.lock  README.md  .github/workflows/ci.yml
crates/lucernad-web/
  Cargo.toml
  src/lib.rs        WebConfig, WebTopics, register, Web, o laço, os métodos
  src/lrc.rs        Line, parse_lrc, plain_lines, LINES_MAX, LINE_CHARS_MAX
  src/matching.rs   Track, normalize, first_artist, same_track
  src/http.rs       Request, Response, HttpError, Http, ReqwestHttp, RetryPolicy, fetch, retry_after
  src/endpoints.rs  Endpoints, base_from_env
  src/cache.rs      Cache, Kind
  src/lyrics.rs     Source, Lyrics, Found, LyricsEntry, fresh, find e os pedidos
  src/weather.rs    Location, Current, Day, Forecast, City, forecast, search_cities e os parses
  src/net.rs        Link, link_from, Connectivity, NmConnectivity
  src/testing.rs    (feature "testing") LocalServer, FakeHttp, FakeLink, fixtures, fake_config, quiet_config, isolated
  tests/{lrc,matching,http,endpoints,cache,sources,net,module,sonda}.rs  tests/isolado.sh
crates/lucernad/  Cargo.toml  src/lib.rs  tests/{common/mod.rs,protocol.rs,process.rs}
```

**`lucerna-shell`:** `services/Web.qml` (novo), `services/Lyrics.qml` e `services/Weather.qml` (reescritos), `core/config/Config.qml` (`vagalumeKey`), `core/widgets/Icons.qml` (`sunrise`, `sunset`), `features/dashboard/state/{DashboardState,MediaState,WeatherState,OverviewState}.qml`, `features/dashboard/ui/{MediaPage,WeatherPage}.qml` (a `WeatherPage` reescrita), `features/settings/state/SettingsState.qml`, `features/settings/ui/{DashboardPage,AboutPage}.qml`, `features/settings/ui/{CreditsPage,Settings}.qml` (a página nova e o registro), `core/widgets/Icons.qml` (`credits`), `tests/{web,lyrics,weather,weatherstate,credits}.test.js`, `tests/runner.qml`, `tests/coverage.json`, `ci/{web-fake.py,smoke.sh,tolerated.txt}`, `ci/packages/smoke.txt`, `dev/lucernad.rev`, `README.md`, `Lucerna — Pendências.md`, `Lucerna — Proposta.md`.

---

### Tarefa 1: `lucernad` — o crate, o LRC e o casamento de faixas

Repositório `lucernad`, branch `feat/web` (Global Constraints).

**Files:**
- Modify: `Cargo.toml` (`lucernad-web`)
- Create: `crates/lucernad-web/Cargo.toml` (`description = "Módulo web do lucernad: a letra das músicas e a previsão do tempo, com cache em disco."`, `[features] testing = []`, ele mesmo com `testing` em `[dev-dependencies]`; `unicode-normalization` e `serde_json`, já aqui: o `lines_json` é desta tarefa), `src/lib.rs` (só os `pub mod` desta tarefa e a doc do crate), `src/{lrc,matching}.rs`
- Test: `crates/lucernad-web/tests/{lrc,matching}.rs`

**Interfaces:**
- Consumes: nada.
- Produces:
  - `lrc`: `pub const LINES_MAX: usize = 1000; pub const LINE_CHARS_MAX: usize = 200; pub const TEXT_BYTES_MAX: usize = 128 * 1024;` `#[derive(Clone, Debug, PartialEq)] pub struct Line { pub time: Option<f64>, pub text: String }`; `pub fn parse_lrc(lrc: &str) -> Vec<Line>` (Decisões; só linhas com marca de tempo válida); `pub fn plain_lines(text: &str) -> Vec<Line>` (`time` `None`; `\r\n` aceito; linhas vazias do fim tiradas; as do meio ficam como `""`); `pub fn lines_json(lines: &[Line]) -> serde_json::Value` (`[{time, text}]`, `null` sem tempo). Os três limites valem nas duas funções (corte em fronteira de caractere; a linha que passaria do `TEXT_BYTES_MAX` cai, com as seguintes).
  - `matching`: `#[derive(Clone, Debug, PartialEq)] pub struct Track { pub artist: String, pub title: String, pub album: String, pub duration: Option<f64> }` com `pub fn key(&self) -> String` (Decisões); `pub const DURATION_TOLERANCE: f64 = 2.0`; `pub fn normalize(s: &str) -> String` (Decisões, sem os sufixos); `pub fn title_key(s: &str) -> String` (normaliza e tira os sufixos); `pub fn first_artist(s: &str) -> String` (normalizado); `pub fn same_track(want: &Track, title: &str, artist: &str, duration: Option<f64>) -> bool` (títulos pelo `title_key`; artistas pelas Decisões; duração só quando os dois lados têm, `|a − b| ≤ 2`).

- [ ] **Passo 1: os testes.**
  - `tests/lrc.rs`:
    - `several_stamps_on_one_line`: `"[00:12.00][01:30.50]refrão\n[00:20]outra"` → `[(12.0, "refrão"), (20.0, "outra"), (90.5, "refrão")]`.
    - `offset_shifts_every_line`: `"[offset:+500]\n[00:10.00]a"` → 9,5; `"[offset:-1000]\n[00:10.00]a"` → 11,0; `"[offset:+20000]\n[00:10.00]a"` → 0,0.
    - `empty_lines_are_pauses`: `"[00:01.00]a\n[00:05.00]\n[00:09.00]b"` → três linhas, a do meio `""`.
    - `metadata_and_garbage_are_ignored`: `"[ar:Artista]\n[ti:x]\nsem tempo\n[99:aa]x\n[00:02.5]ok"` → só `(2.5, "ok")`; `"[100:00.00]a"` → 6000,0; `"[00:01.234]a"` → 1,234.
    - `limits_hold`: 5000 linhas → 1000; uma linha de 10 000 `"é"` → 200 caracteres; 1000 linhas de 200 `"€"` (600 bytes cada) → a soma dos textos ≤ `TEXT_BYTES_MAX` (218 linhas); o mesmo no `plain_lines`.
    - `control_characters_are_removed`: `"[00:01]a\u{1b}[2Jb"` → `"a[2Jb"`.
    - `plain_lyrics`: `plain_lines("a\r\n\r\nb\n\n")` → `[(None, "a"), (None, ""), (None, "b")]`; `lines_json` dela → `[{"time":null,"text":"a"},…]`.
  - `tests/matching.rs`:
    - `accents_case_and_punctuation`: `normalize("Ação, É Já!")` = `"acao e ja"`; `normalize("  AC/DC ")` = `"ac dc"`.
    - `known_suffixes_are_dropped`: `title_key` de `"Faroeste Caboclo - Remastered 2011"`, `"Faroeste Caboclo (Ao Vivo)"`, `"Faroeste Caboclo [Live]"`, `"Faroeste Caboclo - Ao Vivo"`, `"Faroeste Caboclo (feat. Fulano)"` e `"Faroeste Caboclo ft. Fulano"` = `"faroeste caboclo"`; `"Tempo Perdido (Instrumental)"` = `"tempo perdido instrumental"`; `"Música (Parte 2)"` = `"musica parte 2"`.
    - `suffix_words_are_whole_words`: as palavras dos sufixos só valem inteiras: `"Recomeço (Começo)"` = `"recomeco comeco"` (`com` dentro de `começo`); `"Música - Parte Final"` = `"musica parte final"` (`part` dentro de `parte`); `"Música (Liveira)"` = `"musica liveira"`; `"Música (Edição Especial)"` fica com `edicao especial`.
    - `first_artist_splits_guests`: `"Anitta, Cardi B"` → `"anitta"`; `"Chitãozinho & Xororó"` → `"chitaozinho"`; `"Skank feat. Fulano"` → `"skank"`.
    - `same_track_accepts_and_refuses`: `want` = `Track { artist: "Legião Urbana", title: "Tempo Perdido - Remastered", album: "", duration: Some(201.0) }`: aceita `("Tempo Perdido", "Legião Urbana", Some(202.5))` e `("tempo perdido", "LEGIAO URBANA", None)`; recusa `("Tempo Perdido", "Outra Banda", None)`, `("Tempo Perdido", "Legião Urbana", Some(204.0))` e `("Tempo", "Legião Urbana", None)`.
    - `guests_match_the_main_artist`: `want.artist` `"Anitta, Cardi B"` casa com `"Anitta"` e não com `"Cardi B"`; `"Chitãozinho & Xororó"` casa com `"Chitãozinho & Xororó"`.
    - `key_is_normalized_and_rounded`: a `key` de `("Legião Urbana", "Tempo Perdido", Some(201.4))` = a de `("LEGIAO URBANA", "Tempo Perdido (Remastered)", Some(200.6))`; sem duração termina em `"\u{1f}-"`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-web` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-web --test lrc --test matching` → 14 passed; o workspace inteiro verde; a prova sem rede das Global Constraints → verde (o crate novo entra na suíte; se falhar pelo `unshare` e não pelo código, pare e relate).
- [ ] **Passo 5: commit.** `git commit -m "Rede: o LRC e o casamento de faixas"`

---

### Tarefa 2: `lucernad` — o cliente HTTP, a nova tentativa e as fontes de teste

**Files:**
- Create: `crates/lucernad-web/src/{http,endpoints}.rs`; em `src/testing.rs`, `LocalServer` e `FakeHttp`
- Modify: `Cargo.toml` e `crates/lucernad-web/Cargo.toml` (`reqwest`, `rustls`, `tokio` com `io-util`, `serde_json`)
- Test: `crates/lucernad-web/tests/{http,endpoints}.rs`

**Interfaces:**
- Consumes: nada.
- Produces:
  - `http`: `pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>`; `pub const REQUEST_TIMEOUT: Duration` (10 s), `BODY_MAX: usize = 4 * 1024 * 1024`, `USER_AGENT: &str` (Decisões, com `env!("CARGO_PKG_VERSION")`); `#[derive(Clone, PartialEq, Eq)] pub struct Request { pub url: String, pub query: Vec<(String, String)> }` com `new(url: impl Into<String>) -> Request` e `param(self, k: &str, v: impl Into<String>) -> Request`, e um `Debug` à mão que mostra `apikey=***`; `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Response { pub status: u16, pub retry_after: Option<Duration>, pub body: Vec<u8> }`; `#[derive(Clone, Debug, PartialEq, Eq)] pub enum HttpError { Timeout, Connect, Status(u16), TooLarge, WaitTooLong(u64), Other(String) }` com `message(&self) -> String` (os textos `<e>` do Formato; `Other` já vem sem URL); `pub trait Http: Send + Sync + 'static { fn get<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>>; }` (uma tentativa; qualquer status volta como `Ok`); `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Network { Internet, Loopback }`; `pub struct ReqwestHttp` com `pub fn new(timeout: Duration, network: Network) -> Result<ReqwestHttp, String>` (`Internet`: `https_only(true)` e a política de redirecionamento pelo `same_host_redirect`; `Loopback`: `http` aceito e `redirect::Policy::none()`; o erro do `reqwest` sempre pelo `without_url()`); `pub fn same_host_redirect(from: &str, to: &str, hops: usize) -> bool` (mesmo host e esquema `https`, `hops` < 3); `#[derive(Clone, Copy, Debug)] pub struct RetryPolicy { pub retries: usize, pub base: Duration, pub retry_after_max: Duration }` com `standard()` (3, 1 s, 60 s) e `short()` (1, 1 s, 5 s, o do `web.searchCity`); `pub async fn fetch(http: &dyn Http, req: &Request, policy: &RetryPolicy) -> Result<Response, HttpError>` (Decisões; esgotadas as tentativas, o último erro, com `Status(n)` para 429/5xx); `pub fn retry_after(value: &str) -> Option<Duration>` (só segundos inteiros ≥ 0).
  - `endpoints`: `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Endpoints { pub lrclib: String, pub vagalume: String, pub forecast: String, pub geocoding: String }` com `real()` (`https://lrclib.net`, `https://api.vagalume.com.br`, `https://api.open-meteo.com`, `https://geocoding-api.open-meteo.com`) e `under(base: &str)` (Decisões); os caminhos (`/api/get`, `/api/search`, `/search.php`, `/v1/forecast`, `/v1/search`) são somados por quem pede; `pub fn base_from_env(value: Option<&OsStr>) -> Result<Option<String>, String>` (Decisões; o `Err` é o texto do Formato).
  - `testing::LocalServer` (HTTP/1.1 mínimo sobre `tokio::net::TcpListener` em 127.0.0.1:0, `Connection: close`, numa tarefa do runtime do teste): `start(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> LocalServer` (async), `base(&self) -> String`, `seen(&self) -> Vec<Seen>`; `#[derive(Clone, Debug)] pub struct Seen { pub path: String, pub query: Vec<(String, String)>, pub user_agent: String }` (a query decodificada); `pub struct Reply { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8>, pub delay: Duration, pub drop: bool }` com `json(status, &Value)`, `text(status, &str)`, `bytes(status, Vec<u8>)`, `after(self, Duration)`, `header(self, k, v)`, `dropped()` (fecha sem responder); `testing::closed_port() -> String` (uma base num porto que ninguém escuta).
  - `testing::FakeHttp` (`Clone`; `impl Http`): `new()`, `route(&self, prefix: &str, f: impl Fn(&Request) -> Result<Response, HttpError> + Send + Sync + 'static)` (vale a rota cujo prefixo começa a `url`; registrar de novo o mesmo prefixo **substitui** a anterior, para um teste trocar uma resposta das rotas padrão; entre prefixos diferentes, o mais longo; sem rota → `Err(Connect)`), `requests(&self) -> Vec<Request>`, `count(&self) -> usize`, `hold(&self)` / `release(&self)` (os pedidos ficam parados até o `release`), `in_flight(&self) -> usize`, `max_in_flight(&self) -> usize` (o pico de pedidos simultâneos; um pedido abortado no meio sai da conta na hora, pelo `Drop` do futuro), `panicking() -> FakeHttp`; `testing::json(status: u16, v: Value) -> Response`.

- [ ] **Passo 1: os testes.**
  - `tests/http.rs` (o `ReqwestHttp` de verdade em `Network::Loopback` contra o `LocalServer`; `fast()` = `RetryPolicy { retries: 3, base: 10 ms, retry_after_max: 2 s }`):
    - `sends_the_user_agent_and_query`: `get(base + "/x", [("a", "b c")])` → `Seen { path: "/x", query: [("a", "b c")], user_agent: USER_AGENT }`, `Response` 200 com o corpo.
    - `timeout_is_its_own_error`: resposta com `after(2 s)`, `ReqwestHttp::new(300 ms, Loopback)` → `Err(Timeout)` em menos de 1 s.
    - `connection_refused`: `closed_port()` → `Err(Connect)`.
    - `body_over_the_limit_is_refused`: corpo de `BODY_MAX + 1` → `Err(TooLarge)`; de `BODY_MAX` → `Ok`.
    - `retries_transient_errors_then_gives_up`: 503, 503, 200 → `fetch` `Ok` com 3 pedidos vistos; 500 sempre → `Err(Status(500))` com 4 pedidos; 404 → `Ok(404)` com 1 pedido; `dropped()` sempre (conexão fechada sem resposta) → `Err(Connect)` com 4 pedidos (o `ReqwestHttp` classifica a conexão fechada antes da resposta como `Connect`).
    - `retry_after_is_respected`: 429 com `Retry-After: 1` e depois 200 → o segundo pedido ≥ 1 s depois do primeiro; `Retry-After: 120` → `Err(WaitTooLong(120))` com 1 pedido.
    - `errors_never_carry_the_url`: o pedido com `param("apikey", "SEGREDO123")` ao `closed_port()` e a um servidor `dropped()` → nem o `{:?}` nem o `message()` do erro contém `"SEGREDO123"` nem `"127.0.0.1"`; o `{:?}` do `Request` contém `"apikey=***"` e não a chave.
    - `loopback_does_not_follow_redirects`: o servidor responde 302 com `Location: /y` → `Ok` com status 302 e só um pedido visto (`/x`).
    - `redirects_only_to_the_same_host`: `same_host_redirect("https://lrclib.net/a", "https://lrclib.net/b", 0)` verdadeiro; para `https://outro.example/b`, para `http://lrclib.net/b` ou com `hops` 3 → falso.
    - `retry_after_parsing`: `"5"` → 5 s; `" 0 "` → 0; `"Wed, 21 Oct 2015 07:28:00 GMT"`, `"-1"`, `""` → `None`.
  - `tests/endpoints.rs`:
    - `real_and_local_bases`: `Endpoints::real().lrclib` = `"https://lrclib.net"` (e as outras três); `under("http://127.0.0.1:8080")` → `"http://127.0.0.1:8080/lrclib"`, `…/vagalume`, `…/forecast`, `…/geocoding`.
    - `test_base_only_on_loopback`: `None` e `""` → `Ok(None)`; `"http://127.0.0.1:8080"`, `"http://[::1]:9"` → `Ok(Some(_))`; `"http://localhost:8080"`, `"https://127.0.0.1:8080"`, `"http://127.0.0.1"`, `"http://127.0.0.1:0"`, `"http://127.0.0.1:8080/x"`, `"http://10.0.0.1:80"`, `"http://127.0.0.1.evil.com:80"`, `"http://user@127.0.0.1:8"`, `"http://127.0.0.1:70000"` → `Err` com "LUCERNAD_WEB_BASE inválido".
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-web --test http --test endpoints` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-web --test http --test endpoints` → 12 passed; o workspace inteiro verde; `cargo tree -p lucernad-web -i openssl-sys` → nada.
- [ ] **Passo 5: commit.** `git commit -m "Rede: o cliente HTTP com prazo, nova tentativa e Retry-After"`

---

### Tarefa 3: `lucernad` — o cache em disco

**Files:**
- Create: `crates/lucernad-web/src/cache.rs`
- Modify: `crates/lucernad-web/Cargo.toml` (`sha2`, `lucerna-paths`, `serde_json`; `tempfile` nos testes)
- Test: `crates/lucernad-web/tests/cache.rs`

**Interfaces:**
- Consumes: nada.
- Produces: `pub const CACHE_LIMIT: u64 = 50 * 1024 * 1024; pub const ENTRY_MAX: usize = 512 * 1024;` `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Kind { Lyrics, Weather }` (as pastas `lyrics` e `weather`); `#[derive(Clone, Debug)] pub struct Cache` com `pub fn new(dir: PathBuf, limit: u64) -> Cache` (a pasta só é criada na primeira escrita), `pub fn get(&self, kind: Kind, key: &str) -> Option<(SystemTime, Value)>` (o `savedAt` com resolução de segundo), `pub fn put(&self, kind: Kind, key: &str, value: &Value, now: SystemTime) -> io::Result<()>` (maior que `ENTRY_MAX` → não grava e volta `Ok`; depois, a poda), `pub fn remove(&self, kind: Kind, key: &str)`, `pub fn path(&self, kind: Kind, key: &str) -> PathBuf`. Arquivos `.tmp-*` com mais de 1 h são apagados na poda.

- [ ] **Passo 1: os testes** (`tests/cache.rs`, num `TempDir`).
  - `round_trip_with_saved_time`: `put(Lyrics, "k", {"a":1}, t0)` → `get` = `(t0 no segundo, {"a":1})`; `get(Weather, "k")` → `None`.
  - `keys_become_hashed_file_names`: a chave `"Artista/../x"` → arquivo em `<dir>/lyrics/` com 64 dígitos hexadecimais e `.json`; a pasta `<dir>` com modo 0700.
  - `corrupt_entry_is_ignored_and_removed`: lixo no arquivo → `get` `None` e o arquivo apagado; um JSON sem `savedAt` → o mesmo.
  - `big_entry_is_not_written`: um valor com 600 KiB → `put` `Ok`, `get` `None`.
  - `stays_under_the_limit`: limite 10 KiB, 20 entradas de ~1 KiB com `mtime` crescente → a soma dos `.json` ≤ 10 KiB, a última presente e a primeira apagada.
  - `unwritable_dir_is_not_fatal`: um arquivo comum no lugar da pasta → `put` `Err`, `get` `None`, sem pânico.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-web --test cache` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces (Decisões: escrita atômica, leitura limitada, poda por `mtime`).
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-web --test cache` → 6 passed; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Rede: o cache em disco, com limite e escrita atômica"`

---

### Tarefa 4: `lucernad` — as fontes: LRCLIB, Vagalume e Open-Meteo

**Files:**
- Create: `crates/lucernad-web/src/{lyrics,weather}.rs`; em `src/testing.rs`, as respostas de exemplo (`lrclib_entry`, `forecast_body`, `cities_body`, `vagalume_body`)
- Test: `crates/lucernad-web/tests/sources.rs`

**Interfaces:**
- Consumes: `Line`, `parse_lrc`, `plain_lines`, `lines_json` (Tarefa 1); `Track`, `same_track` (Tarefa 1); `Http`, `Request`, `fetch`, `RetryPolicy`, `HttpError` (Tarefa 2); `Endpoints` (Tarefa 2).
- Produces:
  - `lyrics`: `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Source { Lrclib, Vagalume }` com `as_str()` (`"lrclib"`, `"vagalume"`); `#[derive(Clone, Debug, PartialEq)] pub struct Lyrics { pub synced: bool, pub lines: Vec<Line>, pub source: Source, pub url: Option<String> }`; `#[derive(Clone, Debug, PartialEq)] pub enum Found { Lyrics(Lyrics), None { vagalume: bool } }` (o `vagalume` diz se ele foi consultado) com `to_json(&self) -> Value` e `from_json(v: &Value) -> Option<Found>` (o formato do cache); `pub const NONE_TTL: Duration` (7 dias); `pub fn fresh(found: &Found, saved: SystemTime, now: SystemTime, has_key: bool) -> bool` (letra: sempre; nenhuma: até 7 dias e não (`has_key` e `!vagalume`)); `pub fn lrclib_get(ep: &Endpoints, t: &Track) -> Request`, `lrclib_search(ep, t) -> Request`, `vagalume(ep, t, key: &str) -> Request`; `pub async fn find(http: &dyn Http, ep: &Endpoints, policy: &RetryPolicy, t: &Track, key: Option<&str>) -> Result<Found, String>` (a ordem do spec; os `Err` são os textos do Formato). **Decisão:** o `get` com `syncedLyrics` acaba ali; com só `plainLyrics`, a letra de texto fica guardada como reserva e a busca roda atrás de uma sincronizada que case; achou → a sincronizada; não achou (ou a busca falhou) → a de texto do `get`, sem ir ao Vagalume (a LRCLIB já casou título, artista e duração).
  - `weather`: `#[derive(Clone, Debug, PartialEq)] pub struct Location { pub name: String, pub latitude: f64, pub longitude: f64 }` com `place_key(&self) -> String`; `pub struct Current { pub temperature: f64, pub apparent: f64, pub humidity: f64, pub wind: f64, pub code: u32, pub is_day: bool }`; `pub struct Day { pub date: String, pub code: u32, pub min: f64, pub max: f64, pub precipitation: Option<f64>, pub sunrise: Option<String>, pub sunset: Option<String> }`; `pub struct Forecast { pub current: Current, pub daily: Vec<Day>, pub utc_offset: i32 }` (os três com `Clone, Debug, PartialEq`; `Forecast::to_json`/`from_json` para o cache, com todos os dias); `pub fn view(&self, now: SystemTime) -> Value` (a parte do Formato sem `location`/`state`/`updatedAt`: a data local da cidade = `now + utc_offset`; `daily` só de hoje em diante, sem o nascer/pôr por dia; `sunrise`/`sunset` do dia com essa data, ou `null`); `pub fn forecast_request(ep, loc) -> Request`; `pub fn parse_forecast(body: &[u8]) -> Result<Forecast, String>` (até 7 dias; listas de tamanhos diferentes, número não finito, data fora de `AAAA-MM-DD` ou hora fora de `AAAA-MM-DDTHH:MM` → `Err` "O Open-Meteo respondeu fora do formato."); `pub async fn forecast(http, ep, policy, loc) -> Result<Forecast, String>`; `#[derive(Clone, Debug, PartialEq)] pub struct City { pub name: String, pub state: String, pub country: String, pub country_code: String, pub latitude: f64, pub longitude: f64 }` com `to_json`; `pub fn geocoding_request(ep, query: &str) -> Request`; `pub fn parse_cities(body: &[u8]) -> Result<Vec<City>, String>`; `pub async fn search_cities(http, ep, policy, query) -> Result<Vec<City>, HttpOrFormat>` com `pub enum HttpOrFormat { Http(HttpError), Format }` (o módulo transforma em `1002`/`1003`).

- [ ] **Passo 0: a doc do Vagalume.** Leia (só leitura, `WebFetch`) `https://api.vagalume.com.br/docs/letras/` e `https://api.vagalume.com.br/terms/`. Confirme os valores de `type`, o campo da URL da página e o crédito pedido. Se a doc confirmar `exact`/`aprox`, siga as Decisões. Se trouxer outra coisa, ajuste os testes desta tarefa ao que ela diz e anote para o PR. Se ainda estiver fora, siga as Decisões e anote. O crédito pedido vai para o aviso do README (Tarefa 11a).
- [ ] **Passo 1: os testes** (`tests/sources.rs`, com o `FakeHttp`; `ep` = `Endpoints::under("http://127.0.0.1:1")`; `want` = `Track { "Legião Urbana", "Tempo Perdido", "Dois", Some(201.0) }`).
  - `lrclib_get_first_with_all_fields`: o pedido a `<lrclib>/api/get` com `track_name`, `artist_name`, `album_name` `"Dois"` e `duration` `"201"`; a resposta com `syncedLyrics` → `Found::Lyrics` sincronizada, `source` `Lrclib`, `url` `None`, 1 pedido.
  - `album_and_duration_are_optional`: álbum `""` e duração `None` → sem `album_name` nem `duration`.
  - `get_miss_falls_back_to_a_checked_search`: o `get` com 404 `TrackNotFound`; a busca (`track_name`, `artist_name`) com `[outra banda sincronizada, a certa só texto, a certa sincronizada com 1,5 s de diferença]` → a certa sincronizada; pedidos: `get`, `search`.
  - `search_refuses_the_wrong_song`: só resultados de outro artista ou com 5 s de diferença → `Found::None { vagalume: false }`, 2 pedidos.
  - `instrumental_or_empty_is_none`: o `get` com `instrumental: true` e sem letra, a busca vazia → `None`.
  - `vagalume_only_with_a_key`: `get` 404, busca `[]`, chave `"CHAVE"` → o terceiro pedido a `<vagalume>/search.php` com `art`, `mus` e `apikey` `"CHAVE"`; a resposta `{"type":"exact","art":{"name":"Legião Urbana"},"mus":[{"name":"Tempo Perdido","url":"https://www.vagalume.com.br/legiao-urbana/tempo-perdido.html","text":"a\nb"}]}` → `Lyrics { synced: false, lines: [(None, "a"), (None, "b")], source: Vagalume, url: Some(aquela) }`; sem chave, nenhum pedido ao Vagalume e `None { vagalume: false }`.
  - `vagalume_approximate_must_still_match`: `"aprox"` com `"Tempo Perdido (Ao Vivo)"` → letra; com `"Outra Música"` → `None { vagalume: true }`; `"song_notfound"` e `"notfound"` → `None { vagalume: true }`; `url` `"/legiao-urbana/x.html"` → `"https://www.vagalume.com.br/legiao-urbana/x.html"`; `"https://evil.example/x"` → `"https://www.vagalume.com.br/"`.
  - `network_error_is_an_error_not_none`: o `get` sempre `Err(Connect)` → `Err` começando por "Não consegui falar com a LRCLIB"; a busca com 500 → `Err`; o Vagalume com `Err(Timeout)` → `Err` com "Vagalume", sem `"CHAVE"` no texto.
  - `captive_portal_html_is_an_error`: o `get` com 200 e `"<html>…"` → `Err("A LRCLIB respondeu fora do formato.")`; o mesmo no Vagalume, com o texto dele.
  - `none_expires_and_a_new_key_retries`: `fresh(None{false}, t, t + 6 dias, false)` verdadeiro; `+ 8 dias` falso; `fresh(None{false}, t, t, true)` falso; `fresh(None{true}, t, t, true)` verdadeiro; uma letra com `+ 400 dias` verdadeiro; `Found::from_json(&f.to_json())` = `Some(f)` para os dois casos.
  - `forecast_request_and_parse`: o pedido a `<forecast>/v1/forecast` com `latitude`, `longitude`, `current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m,is_day`, `daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset`, `timezone=auto`, `forecast_days=7`; o `forecast_body()` → 7 dias, `daily[0].sunrise` `Some("2026-10-24T05:41")`, `utc_offset` −14400; com 10 dias → 7; `sunrise`/`sunset` `null` na resposta → `None` (sem erro); listas de tamanhos diferentes → `Err`; HTML → `Err`.
  - `past_days_are_dropped_by_the_city_date`: o `forecast_body()` (24 a 30 de outubro, offset −4 h) visto em 2026-10-26T02:00Z (ainda dia 25 na cidade) → `daily` de 25 a 30 e `sunrise` o do dia 25; visto em 2026-10-31T12:00Z → `daily` `[]`, `sunrise`/`sunset` `null`.
  - `plain_get_still_looks_for_synced`: o `get` só com `plainLyrics` e a busca com uma sincronizada que casa → a sincronizada (2 pedidos); a busca sem nada que case → a de texto do `get`, `source` `Lrclib`; a busca com 500 → a de texto do `get` (não é erro).
  - `cities_search`: o pedido a `<geocoding>/v1/search` com `name`, `count=5`, `language=pt`, `format=json`; 6 resultados → 5 com `state` = `admin1`, `country_code` → `country_code`; sem a chave `results` → `[]`; sem `admin1` → `""`; um nome de 300 caracteres → 100.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-web --test sources` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-web --test sources` → 14 passed; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Rede: a LRCLIB, o Vagalume e o Open-Meteo, com casamento conferido"`

---

### Tarefa 5: `lucernad` — a conectividade pelo NetworkManager

**Files:**
- Create: `crates/lucernad-web/src/net.rs`; em `src/testing.rs`, `FakeLink` e `isolated`; `tests/isolado.sh`
- Modify: `crates/lucernad-web/Cargo.toml` (`zbus`)
- Test: `crates/lucernad-web/tests/{net,sonda}.rs`

**Interfaces:**
- Consumes: nada.
- Produces: `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Link { Online, Offline, Unknown }`; `pub fn link_from(connectivity: u32) -> Link` (Decisões); `pub trait Connectivity: Send + Sync + 'static { fn watch(&self) -> tokio::sync::watch::Receiver<Link>; }` (chame dentro do runtime; a primeira chamada começa a tarefa que acompanha, as outras dividem o mesmo `watch`); `pub struct NmConnectivity` com `system()` (o D-Bus do sistema, preguiçoso; sem barramento ou sem o nome → `Unknown`, e a cada 30 s tenta conectar de novo; o `NameOwnerChanged` do `org.freedesktop.NetworkManager` relê); `testing::FakeLink` (`Clone`, `impl Connectivity`): `new(link: Link)`, `set(&self, link: Link)`; `testing::isolated()` (a trava da energia, com o texto "sonda.rs: só roda pelo tests/isolado.sh, num container").

**`tests/isolado.sh`**: o da energia (`crates/lucernad-power/tests/isolado.sh`), com o cabeçalho deste módulo, a config do `dbus-daemon` permitindo o nome `org.freedesktop.NetworkManager` e o `cargo test -p lucernad-web --test sonda --locked -- --ignored --test-threads=1`.

- [ ] **Passo 1: os testes.**
  - `tests/net.rs` (rodam no CI): `connectivity_values` (0 → `Unknown`, 1 → `Offline`, 2, 3 e 4 → `Online`, 9 → `Unknown`); `fake_link_delivers` (`set(Offline)` chega ao `watch`); `sonda_refuses_outside_the_container` (sem `LUCERNA_ISOLADO`, `catch_unwind(isolated)` → `Err`).
  - `tests/sonda.rs` (`#[ignore = "só no D-Bus isolado (tests/isolado.sh)"]`, começando por `isolated()`; um NetworkManager falso por `#[zbus::interface]` com a propriedade `Connectivity`): `follows_network_manager` → `Online` com 4; troca para 1 com `PropertiesChanged` → `Offline` em 2 s; solta o nome → `Unknown` em 2 s; registra de novo com 4 → `Online` em 2 s.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-web --test net` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-web --test net` → 3 passed; o workspace inteiro verde; a sonda no container isolado (o `docker run` da energia, com `crates/lucernad-web/tests/isolado.sh` no fim) → 1 passed.
- [ ] **Passo 5: commit.** `git commit -m "Rede: a conectividade pelo NetworkManager"`

---

### Tarefa 6: `lucernad` — o módulo `web`

**Files:**
- Modify: `crates/lucernad-web/src/lib.rs` (o módulo), `src/testing.rs` (`fake_config`, `quiet_config`), `crates/lucernad-web/Cargo.toml` (`lucernad-core`, `lucerna-service`; nos testes, `lucerna-ipc`)
- Test: `crates/lucernad-web/tests/module.rs`

**Interfaces:**
- Consumes: Tarefas 1–5; `lucernad_core::{supervise, publish_changed, ModuleTask}`; `lucerna_service::{ServerBuilder, Topic, RpcError}`; `lucerna_paths::{AppPaths, Env}`.
- Produces:
  - `pub const TRACK_SETTLE: Duration` (500 ms), `WEATHER_RETRY` (5 min), `DEFAULT_REFRESH: u32 = 30`, `TEXT_MAX: usize = 500`, `QUERY_MAX: usize = 100`, `NAME_MAX: usize = 200`, `KEY_MAX: usize = 128`.
  - `pub struct WebConfig { pub http: Arc<dyn Http>, pub endpoints: Endpoints, pub base_error: Option<String>, pub cache: Option<Cache>, pub link: Arc<dyn Connectivity>, pub policy: RetryPolicy, pub search_policy: RetryPolicy, pub settle: Duration, pub weather_retry: Duration, pub minute: Duration }` (`minute`: quanto vale um minuto do `refresh`, 60 s; encurtado nos testes) com `pub fn system() -> WebConfig` (Decisões: o `base_from_env(env::var_os("LUCERNAD_WEB_BASE").as_deref())`, com o aviso da partida; o cache pela `lucerna-paths`, ou `None` com o aviso `lucernad: sem pasta de cache (<e>): a letra e o clima ficam sem cache`; `ReqwestHttp::new(REQUEST_TIMEOUT, Internet ou Loopback)`, cujo erro vira `base_error`; `NmConnectivity::system()`; `RetryPolicy::standard()` e `short()`, `Network::Internet` (ou `Loopback` com a base, e então o cache em `<cache>/web/teste/`) e as constantes).
  - `#[derive(Clone)] pub struct WebTopics { pub lyrics: Topic, pub weather: Topic, pub status: Topic }`.
  - `pub fn register(server: &mut ServerBuilder, cfg: WebConfig) -> Web` (os três tópicos do Formato, nesta ordem, e os quatro métodos); `pub struct Web { pub topics: WebTopics, /* privado */ }` com `pub fn spawn(self) -> ModuleTask` (o `supervise` com `"de rede"` e `{"available":false,"error":"O módulo de rede parou por um erro interno.","online":false}`).
  - `testing::fake_config(dir: &Path) -> (WebConfig, Fakes)` com `pub struct Fakes { pub http: FakeHttp, pub link: FakeLink }` (o cache em `dir/cache` com `CACHE_LIMIT`, `Endpoints::under("http://127.0.0.1:1")`, `FakeLink::new(Unknown)`, `RetryPolicy { 1, 1 ms, 50 ms }` nas duas políticas, `settle` 20 ms, `weather_retry` 100 ms, `minute` 10 ms) e as rotas padrão do `FakeHttp` (`lrclib_entry` para "Tempo Perdido"/"Legião Urbana", 404 para o resto, `forecast_body_from(hoje)` (a mesma previsão gerada a partir da data local de hoje: os testes do módulo não envelhecem), `cities_body()`); `testing::forecast_body() -> Vec<u8>` (24 a 30 de outubro de 2026, fixo, para a Tarefa 4) e `testing::forecast_body_from(first: &str) -> Vec<u8>` (`AAAA-MM-DD`, sete dias seguidos); `testing::quiet_config(dir: &Path, base: &str) -> WebConfig` (o `ReqwestHttp` de verdade em `Network::Loopback` com `Endpoints::under(base)`, `FakeLink::new(Unknown)`, cache em `dir`), para o binário.

O laço (o que as assinaturas não determinam):

```
partida: base_error → status {false, base_error, false}; os métodos respondem 1004; nada roda
pânico: as buscas rodam em tarefas próprias; o laço espera cada JoinHandle e, num JoinError de pânico, relança com std::panic::resume_unwind(e.into_panic()), para o supervise ver o pânico (um abort não é pânico: ignorado)
estado: settings (None até o 1º configure = offline), a faixa, o link (watch), uma busca de letra e uma de clima em voo (cada uma um JoinHandle abortável, com a key/place_key de quem pediu)
online = settings com offline falso && link != Offline;  status publicado a cada mudança: {available, error, online}

letra (só com assinante de web.lyrics):
  rotina(faixa): aborta a busca em voo;
    sem título ou sem artista → {key, state:"none"}
    cache: fresh(…) → publica (found ou none) e acaba; nunca passa por "searching"
    !online → {state:"error", error: offline ou sem conexão}
    senão publica {state:"searching"}; espera settle (outra troca no meio recomeça); find():
      Found → cache.put + publica; Err → publica {state:"error"} (sem cache)
    toda publicação confere que a key ainda é a da faixa atual
  web.track → guarda a faixa; com assinante, rotina; sem assinante, nada
  ganhar assinante → rotina(faixa atual); perder → aborta a busca em voo
  configure (online agora, ou chave nova), link volta a Online, web.refresh → rotina se o estado é error (ou none com a chave nova)

clima:
  configure sem location → {location:null, state:"none", …}; location nova → aborta a busca em voo e publica o guardado (state "ready", updatedAt dele) ou {state:"loading"} sem guardado
  com assinante e (sem guardado ou guardado mais velho que refresh × minute) e online → publica "loading" mantendo os dados, forecast():
      Ok → cache.put + publica "ready" com updatedAt = agora; próximo em refresh × minute
      Err → publica "error" mantendo os dados; próximo em min(weather_retry, refresh × minute)
    só publica se a place_key ainda é a da location atual
  !online sem guardado → {state:"error", error: offline ou sem conexão}; com guardado → "ready", sem busca
  web.refresh → forecast() já, com ou sem assinante (com location e online)
  ao perder o assinante, aborta a busca e para o relógio

web.searchCity (com RetryPolicy::short() da config, para caber no CALL_TIMEOUT de 30 s): offline (ou sem configure) → 1001; link Offline → 1002; Http → 1003 com a mensagem; formato → 1003 "…: a resposta veio fora do formato"; ok → {results}
```

Os métodos falam com o laço por um `mpsc` com resposta `oneshot`; o `web.searchCity` roda fora do laço (só lê o `online` e as `endpoints`).

- [ ] **Passo 1: os testes** (`tests/module.rs`, com os ajudantes do `tests/module.rs` da energia: `serve(cfg)` guardando o `ModuleTask`, `client(&path)`, `wait_until`, `wait_seq`; `online(&c)` = `web.configure {offline: false}`; `track(&c, artista, título, duração)`).
  - `handshake_lists_topics_and_methods`: `topics` `["web.lyrics", "web.weather", "web.status"]`, `methods` `["web.track", "web.configure", "web.searchCity", "web.refresh"]`.
  - `params_are_validated`: um caso por texto de `-32602` do Formato (`track {"artist":1,…}`, título de 501 caracteres, `duration` -1 e `"5"`, `configure {}` sem `offline`, `vagalumeKey` `"tem espaço"` (a mensagem não contém `"tem espaço"`), `location` com latitude 91, `refresh` 4, `searchCity {"query":""}`), cada um com a mensagem exata.
  - `nothing_goes_out_before_configure`: assina `web.lyrics` e `web.weather`, manda a faixa → letra `{state:"error", error:"Modo offline: a letra não é buscada."}`; `searchCity` → `1001`; `http.count()` 0; o status `{true, null, false}`.
  - `lyrics_found_and_cached`: `online`; assina; a faixa "Tempo Perdido" → `searching` e depois `found` com `source` `lrclib`, `synced` e a `key` = `Track::key()`; um módulo novo sobre o mesmo `dir` → `found` sem nenhum pedido.
  - `cached_track_never_passes_through_searching`: com a letra no cache, a troca para ela publica `found` direto (o `wait_seq` não vê `searching` com aquela `key`).
  - `none_is_cached_and_failure_is_not`: uma faixa que não existe → `none`; voltar a ela → `none` sem pedido novo; com o `get` em `Err(Connect)` → `error` "Não consegui falar com a LRCLIB: não consegui conectar."; voltar a ela → pede de novo.
  - `skipping_tracks_never_shows_old_lyrics`: `settle` 200 ms neste teste; dez faixas trocadas a cada 50 ms, a última "Tempo Perdido" → o estado final é `found` com a `key` da última; nenhum `found` publicado com outra `key`; `max_in_flight()` = 1; no máximo 1 `get` da última faixa e nenhum pedido das nove anteriores.
  - `lyrics_only_with_subscriber`: a faixa sem assinante → 0 pedidos; assinando → 1 `get`.
  - `vagalume_key_never_leaks`: `configure {offline: false, vagalumeKey: "CHAVE-SECRETA"}`; uma faixa que a LRCLIB não tem e o Vagalume com `Err(Connect)` → `error`; nenhum estado de nenhum tópico guardado pelo `wait_seq` nem o `status` contém `"CHAVE-SECRETA"`.
  - `weather_cached_is_published_at_once_and_refreshed_in_background`: o cache com a previsão de Cacaulândia (`forecast_body_from(hoje)`) salva há 1 h (`refresh` 30 → 300 ms no relógio do teste); `hold()`; assina → a primeira publicação é `ready` com o `updatedAt` velho e os 7 dias; depois `loading`; `release()` → `ready` com `updatedAt` novo.
  - `fresh_weather_is_not_refetched`: salva agora → 0 pedidos em 200 ms com assinante.
  - `weather_error_keeps_the_old_data`: guardada velha, a previsão com `Err(Status(500))` → `{state:"error", error:"Não consegui falar com o Open-Meteo: o servidor respondeu 500.", current: …}`; outra tentativa depois do `weather_retry`.
  - `offline_and_no_link_send_nothing`: `configure {offline: true, location}`, assina os dois, faixa, `refresh`, `searchCity` → 0 pedidos; o clima `error` com "Modo offline: o clima não é atualizado." (sem guardado); `offline: false` com `link.set(Offline)` → 0 pedidos e `searchCity` `1002`; `link.set(Online)` → a previsão é pedida e a letra em erro refeita.
  - `going_offline_mid_fetch_sends_nothing_more`: `hold()`; a previsão em voo; `configure {offline: true}`; `release()` → nenhuma publicação `ready` depois do `configure`, e `count()` não cresce em 300 ms.
  - `changing_city_mid_fetch_publishes_only_the_new_one`: `hold()` com a previsão de A em voo; `configure` com B; `release()` → nenhum estado com `location` B e os dados de A; o cache de B não tem os dados de A.
  - `city_search_results`: `online`; `searchCity {"query":"Cacau"}` → 3 resultados com `name`, `state`, `country`, `countryCode`, `latitude`, `longitude`; a rota com `Err(Timeout)` → `1003` "Não consegui falar com o Open-Meteo: o servidor demorou demais para responder.".
  - `refresh_fetches_now_without_subscriber`: `online` com `location`, sem assinante, `web.refresh` → 1 pedido de previsão e o cache gravado.
  - `status_follows_configure_and_link`: `{true, null, false}` antes; `online` → `online` true; `link.set(Offline)` → false.
  - `bad_base_makes_the_module_unavailable`: `base_error` `Some(texto)` → status `{false, texto, false}`; `searchCity`, `track` e `configure` → `1004`; 0 pedidos.
  - `panic_marks_the_module_unavailable`: `FakeHttp::panicking()` com a letra pedida → status com o erro interno; um cliente novo ainda faz o aperto de mão.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-web --test module` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces e o laço.
- [ ] **Passo 4: verificar.** `TMPDIR=/tmp cargo test -p lucernad-web --test module` → 20 passed, 5 vezes seguidas sem falha intermitente; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Rede: o módulo web, com a letra, o clima e a busca de cidade"`


---

### Tarefa 7: `lucernad` — o módulo no binário, a linha de comando e o README

**Files:**
- Modify: `crates/lucernad/{Cargo.toml,src/lib.rs}`, `crates/lucernad/tests/{common/mod.rs,protocol.rs,process.rs}`, `README.md`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `lucernad_web::{register, WebConfig, WebTopics}`, `testing::{quiet_config, LocalServer}` (o `lucernad-web` com `testing` nas `[dev-dependencies]` do binário); `lucerna_cli::{CliCommand, Call}`.
- Produces:
  - `Cargo.toml`: `default = ["system", "spectrum", "clipboard", "power", "web"]`, `web = ["dep:lucernad-web", "dep:lucernad-core", "dep:serde_json"]` (comentário `# O módulo web: a letra das músicas e a previsão do tempo.`); `description` com "(system, spectrum, clipboard, power e web)".
  - `Modules` ganha `#[cfg(feature = "web")] pub web: WebConfig` (`real()`: `WebConfig::system()`); `Lucernad` ganha `web: WebTopics` e `web_task: ModuleTask`; as listas `any(feature = …)` ganham `"web"`; o `main` guarda `_web_task`; o `plain()` passa a `cfg(any(feature = "clipboard", feature = "power", feature = "web"))`; a descrição do `cli()` ganha "…, o brilho das telas e a letra e o clima para o shell.".
  - Com `web`, os `CliCommand` `faixa`, `cidade` e `rede` (Decisões). `faixa`: `about` "Diz ao módulo de rede qual faixa está tocando (a letra é buscada com assinante de web.lyrics)."; `SEGUNDOS` inteiro de 0 a 86400 (`Err("SEGUNDOS precisa ser um inteiro entre 0 e 86400")`). `cidade`: `about` "Procura uma cidade no Open-Meteo (até 5 resultados).". `rede`: `about` "Configura o módulo de rede: offline ou não, a cidade do clima e a atualização; a chave do Vagalume vem do LUCERNAD_VAGALUME_KEY. Substitui a configuração do shell até ele reconectar."; `--lat`/`--lon` números; `--cidade` sem os dois (ou o contrário) → `Err("Passe --cidade, --lat e --lon juntos")`; `--atualizar` de 5 a 1440.
  - CI: o clippy ganha `cargo clippy -p lucernad --no-default-features --features web --all-targets --locked -- -D warnings`.
  - `README.md`: "Situação" com os cinco módulos; em **Módulos**, a seção `web` no formato das outras (o que faz; o que vai para cada serviço (só título, artista, álbum e duração às letras, a cidade e as coordenadas ao Open-Meteo); quando roda (só com assinante; nada antes do `configure`; o modo offline; a conectividade); a tabela dos três tópicos; os métodos e os erros; o cache e o limite; o Vagalume com a chave do usuário e o crédito; o `LUCERNAD_WEB_BASE` dito como só de teste); em **Linha de comando**, `faixa`, `cidade` e `rede`, com a nota de que o `rede` substitui a configuração que o shell mandou (o shell manda a dele de novo só ao reconectar) e de que a chave vem só do ambiente; em **Desenvolvimento**, a sonda do `crates/lucernad-web/tests/isolado.sh` e a prova sem rede (o comando das Global Constraints, com o porquê do segundo `unshare`); em **Licença**/créditos, nada novo.
- [ ] **Passo 1: os testes.**
  - `tests/common/mod.rs`: `Modules { …, web: lucernad_web::testing::quiet_config(dir.path(), "http://127.0.0.1:1") }`, guardando o quinto `ModuleTask`.
  - `protocol.rs`, `the_daemon_serves_the_web_module`: `hello().topics` contém os três `web.*`, `methods` contém `"web.searchCity"`; `web.searchCity {"query":"x"}` → `1001`.
  - `process.rs`: os testes de processo são síncronos; o ajudante guarda um `tokio::runtime::Runtime` vivo junto do `LocalServer` (o `start` roda nele e o servidor morre com ele no fim do teste), um por teste (as rotas `/geocoding/v1/search` com `cities_body()` e o resto 404), e o `lucernad(dir)` passa a pôr `LUCERNAD_WEB_BASE` = a base dele em todo processo; em `cli_help_status_and_exit_codes`: o log de partida contém "LUCERNAD_WEB_BASE=… a rede é de teste"; `lucernad cidade Cacau` antes do `rede` → código diferente de 0 com "Modo offline ligado"; `lucernad rede` → 0; `lucernad cidade Cacau` → 0 e as linhas `Cacaulândia, Rondônia, Brasil  (-10.3375, -62.8986)`…; `lucernad rede --cidade X` → código de uso com "Passe --cidade, --lat e --lon juntos"; `lucernad faixa "Legião Urbana" "Tempo Perdido" 201` → 0; `lucernad faixa A B 90000` → código de uso; com `LUCERNAD_WEB_BASE=https://example.com` → `lucernad --json status` com o `web.status` `available` falso e o texto do Formato; com `LUCERNAD_VAGALUME_KEY=CHAVE-SECRETA lucernad rede` → o log do daemon e a saída não contêm a chave.
  - Unidade em `lib.rs`: `cidade_lines_have_no_control_characters` (uma cidade `"a\u{1b}[2Jb"` com estado `"x\u{202E}y"` → a linha sem caracteres de controle) e `no_city_says_so`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad` → falha.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** O workspace inteiro verde, com os sete clippy das Global Constraints; `grep -rnE "process::Command|tokio::process" crates/*/src` → nada; o `grep` das Global Constraints → só `endpoints.rs` e `sonda.rs`; **a prova sem rede** das Global Constraints → verde (todos os testes passam num namespace sem saída para fora, como o usuário de sempre).
- [ ] **Passo 5: commit.** `git commit -m "lucernad: o módulo web no binário, os subcomandos faixa, cidade e rede e o README"`

---

### Tarefa 8: `lucernad` — conferência na máquina real, PR e merge

Roda depois das Tarefas 9–11. **Quem conduz é o agente**; o usuário é chamado para pôr o Spotify para tocar (se o agente não conseguir ler o que toca), para dar a chave do Vagalume (opcional) e para aprovar a aba Clima. Pedidos de verdade às fontes só nesta tarefa, poucos (dezenas, longe dos limites).

**Files:** nenhum, salvo correções (cada uma com teste e commit próprio).

**Interfaces:**
- Consumes: o binário da Tarefa 7; o shell da `feat/rede-externa` (Tarefas 9–11).
- Produces: `<LUCERNAD_REV>` = o SHA da `main` do `lucernad` depois do merge, para a Tarefa 12.

- [ ] **Passo 1: o daemon à parte.**

  ```sh
  cargo build --release --locked -p lucernad
  R=$(mktemp -d /tmp/lucernad-conf.XXXXXX)
  run() { XDG_RUNTIME_DIR=$R XDG_CACHE_HOME=$R/cache LUCERNA_IDLE_TIMEOUT_MS=600000 target/release/lucernad "$@"; }
  run serve 2> $R/daemon.log & SERVE=$!
  run watch web.lyrics > $R/letra.log & WL=$!; run watch web.weather > $R/clima.log & WW=$!
  ```

  Critério: `run --json status` com `web.status` `{available: true, online: false}` (nada antes do `configure`); `run cidade Cacaulândia` → erro "Modo offline ligado"; `run rede` → `online: true`.
- [ ] **Passo 2: a cidade e o clima.** `run cidade Cacaulândia` → pelo menos um resultado em Rondônia (anote quantos e quais); `run cidade "São José"` → 5 resultados (a lista múltipla). `run rede --cidade "Cacaulândia, Rondônia, BR" --lat <lat> --lon <lon>` (os do resultado) → o `clima.log` com `loading` e depois `ready`, 7 dias, `sunrise`/`sunset` de hoje (conferir que fazem sentido para Rondônia, UTC−4), `current` plausível. **Abre na hora:** `kill $SERVE`; suba de novo o daemon de verdade (**sem** `LUCERNAD_WEB_BASE`, que mandaria o cache para `web/teste/`) com o mesmo `$R/cache`, e `run rede --offline --cidade …` (a falta de rede simulada pelo modo offline; ou o daemon sob `unshare -rn` com o `unshare` aninhado das Global Constraints) → o `clima.log` com `ready` e o `updatedAt` de antes em menos de 1 s, lido do cache real; depois `run rede --cidade …` (online) de novo.
- [ ] **Passo 3: as letras.** Leia a faixa do Spotify pelo MPRIS (`busctl --user get-property org.mpris.MediaPlayer2.spotify /org/mpris/MediaPlayer2 org.mpris.MediaPlayer2.Player Metadata`: `xesam:title`, `xesam:artist`, `xesam:album`, `mpris:length` em µs); se o Spotify não estiver aberto, peça ao usuário para tocar. Para cada faixa, `run faixa "<artista>" "<título>" <segundos> --album "<álbum>"` e confira o `letra.log`: (1) uma internacional com letra sincronizada → `found`, `synced`, `lrclib`; (2) uma brasileira → `found` (anote a fonte); (3) uma com "- Remastered" ou "(Ao Vivo)" no título → `found` pela busca casada; (4) `run faixa "Lucerna Teste" "Faixa Que Não Existe" 200` → `none` e um arquivo novo em `$R/cache/lucerna/web/lyrics/`; (5) `run rede --offline --cidade …` e a (1) de novo → `found` na hora, do cache real (offline, nenhum pedido); volte com `run rede --cidade …`. Com o shell (passo 6), a letra acompanha a música.
- [ ] **Passo 4: o Vagalume (só com a chave do usuário).** **Pare e pergunte** ao usuário se ele quer testar o Vagalume e, se sim, peça a chave (criada em `https://auth.vagalume.com.br/settings/api/`, ou onde a doc lida na Tarefa 4 disser). Sem chave: anote "Vagalume não testado na máquina real (sem chave)" para o PR e siga. Com a chave: a chave **nunca** passa pela conversa nem pela linha de comando: o usuário a grava, no terminal dele, num arquivo 0600 (`install -m 600 /dev/null ~/.cache/lucerna-vagalume.key && read -rs k && printf %s "$k" > ~/.cache/lucerna-vagalume.key`), e o agente usa `LUCERNAD_VAGALUME_KEY="$(cat ~/.cache/lucerna-vagalume.key)" run rede --cidade …`; no fim, `rm ~/.cache/lucerna-vagalume.key` (o usuário decide); uma faixa brasileira que a LRCLIB não tem (o usuário sugere, ou a (4) do passo 3 com uma música real) → `found`, `source` `vagalume`, `url` na página da letra; `grep -cF -f ~/.cache/lucerna-vagalume.key $R/daemon.log $R/letra.log` → 0. No shell do container (passo 6), o usuário cola a chave no campo das Configurações.
- [ ] **Passo 5: offline e sem rede.** `run rede --offline` → `online: false`; `run cidade X` → "Modo offline ligado"; troque de faixa duas vezes e, por 10 s, `ss -tnp | grep "pid=$SERVE"` → nenhuma conexão. A prova sem rede da suíte (Tarefa 7, passo 4) de novo, no commit final.
- [ ] **Passo 6: o shell, no Hyprland do container.** No `lucerna-shell` (branch `feat/rede-externa`): `LUCERNA_TEST_HOST_BUS=1 dev/test.sh up` (para o shell ver o Spotify do host); `docker cp target/release/lucernad lucerna-polish:/usr/bin/lucernad`; `dev/test.sh run 'pkill -x lucernad; sh ~/.config/quickshell/lucerna/ci/power-root.sh /tmp/lucerna-power && setsid -f env LUCERNAD_POWER_ROOT=/tmp/lucerna-power LUCERNA_IDLE_TIMEOUT_MS=86400000 lucernad serve'`; abra a janela para o usuário. Em Configurações → Painel superior, a busca "Cacaulândia" com a lista, a escolha; `dev/test.sh see clima dashboard open weather` → a captura da aba nova, lida e comparada com o `WeatherTab.qml` do Caelestia (cabeçalho com cidade e data e nascer/pôr do sol à direita; card grande; três cards; "Previsão de 7 dias" com sete cards); a aba Mídia com a letra do Spotify (a linha atual acompanha; com o Vagalume, a letra de texto rolável, sem rótulo); o modo offline ligado → o aviso na aba e no card; o `lucernad` parado → "Indisponível". `dev/test.sh errs` sem avisos novos; o usuário aprova. Depois, `dev/test.sh up` sem o `LUCERNA_TEST_HOST_BUS`.
- [ ] **Passo 7: PR.** `kill $WL $WW $SERVE`; `rm -rf $R`; `git push -u origin feat/web`; `gh pr create --repo lucerna-desktop/lucernad --base main --title "lucernad: o módulo de rede externa (letras e clima)" --body <resumo; as dependências novas (sem OpenSSL, sem unsafe no crate); como foi testado (nenhum teste sai para a internet: o LocalServer, o FakeHttp e a prova do unshare; a sonda do NM isolada; a tabela dos passos 1–6 com o que se viu: as faixas, as fontes, o clima de Cacaulândia, o cache sem rede, o offline sem conexões); o que a doc do Vagalume disse (Tarefa 4) e se ele foi testado; as linhas "Para o PR" das Decisões: o LUCERNAD_WEB_BASE, os subcomandos>`. **Controlador:** revisão final da branch (Opus), CI verde, `gh pr merge <n> --repo lucerna-desktop/lucernad --merge`; anote `<LUCERNAD_REV>` (`git fetch && git rev-parse origin/main`); com o usuário, reinstale o `/usr/bin/lucernad` (a memória "lucernad instalado no sistema").

---

### Tarefa 9: `lucerna-shell` — `Web.qml` e `Lyrics.qml` como adaptadores, a letra na aba Mídia

Repositório `lucerna-shell`, branch `feat/rede-externa` (Global Constraints).

**Files:**
- Create: `services/Web.qml`, `tests/web.test.js`
- Modify: `services/Lyrics.qml` (reescrito), `tests/lyrics.test.js` (reescrito), `core/config/Config.qml` (`vagalumeKey`), `features/dashboard/state/{DashboardState,MediaState}.qml`, `features/dashboard/ui/MediaPage.qml:255-300`, `tests/runner.qml` (o `web` nas suítes e `get Web()`), `tests/coverage.json`

**Interfaces:**
- Consumes: `Lucernad.client` (`want`, `call`, `connected`, `received`); `Media` (`title`, `artist`, `album`, `length`); o Formato.
- Produces:
  - `Web` (singleton, dono `"web"`): `property var client: Lucernad.client`; `property string error: ""`; `property bool moduleOnline: false`; `readonly property bool available: client.connected && error === ""`; `readonly property bool online: available && moduleOnline`; `property bool offline: true`, `property string vagalumeKey: ""`, `property var location: null`, `property int refreshMinutes: 30` (ligados pelo `DashboardState`); funções `wantedTopics(): var` (`["web.status"]`), `sync(): void`, `configureParams(): var` (`{offline, vagalumeKey: key ou null, location: {name, latitude, longitude} ou null, refresh: refreshMinutes}`; o `refresh` só vai se for inteiro de 5 a 1440, senão `null`, o padrão do daemon), `configure(): void` (só conectado), `apply(topic: string, data: var): void`, `applyStatus(data: var): void` (`error` = `data.error ?? ""`, `moduleOnline` = `data.online === true`), `connectionChanged(connected: bool): void` (caída: `error` "", `moduleOnline` falso; nos dois casos `sync()`; conectado, `configure()` e depois o sinal `configured()`), `signal configured()`, `refresh(): void` (`web.refresh`), `searchCity(query: string, done: var): void` (`web.searchCity`, a resposta inteira ao `done`); `onOfflineChanged`/`onVagalumeKeyChanged`/`onLocationChanged`/`onRefreshMinutesChanged` → `configure()`; o `Connections` no `client`; `Component.onCompleted: sync()`.
  - `Lyrics` (dono `"lyrics"`): `property var client: Lucernad.client`; `property bool active: false`; `property var lines: []` (`[{time, text}]`, `time` −1 sem tempo); `property bool synced`, `property string state: ""`, `property string source: ""`, `property string url: ""`, `property string error: ""`, `property string key: ""`; `readonly property bool loading: state === "searching"`; `readonly property string trackKey` (artista, título, álbum e duração arredondada do `Media`, juntados com `\u0000`); funções `wantedTopics(active: bool): var` (`["web.lyrics"]` com `active`, `[]` sem), `sync()`, `trackParams(title: string, artist: string, album: string, length: real): var` (`{title, artist, album, duration: length > 0 ? length : null}`), `sendTrack(): void` (só conectado: `web.track` com os do `Media`), `toLine(l: var): var` (`{time: número ? time : -1, text: String(text ?? "")}`), `apply(topic, data)`, `applyLyrics(data: var): void` (`Array.from(data.lines ?? [])` → `toLine`; `url` só se começar por `https://`), `clear(): void`, `connectionChanged(connected: bool): void` (caída: `clear()`; nos dois casos `sync()`; **não** manda a faixa: ela vai no `Web.onConfigured`, para o `web.configure` chegar antes do `web.track` numa conexão nova e a aba não piscar "Modo offline"), `lineAt(position: real): int` e `lineRole(index: int, current: int, synced: bool): string` (as de hoje); `onActiveChanged: sync()`; `onTrackKeyChanged: sendTrack()`; um `Connections` no `Web` (`onConfigured` → `sendTrack()`). Saem `enabled`, `cache`, `get`, `parse`, `request` e o `XMLHttpRequest`.
  - `Config`: `property string vagalumeKey: ""` (comentário: a chave do Vagalume, do próprio usuário; só vai ao lucernad pelo `web.configure`).
  - `DashboardState`: sai só o `Binding` do `Lyrics.enabled` (os do `Weather` velho saem na Tarefa 10); entram `Lyrics.active` = `isShowing("media") && Config.lyricsEnabled` (sem o `!Config.offline`: offline, a letra guardada continua aparecendo, e o daemon não vai à rede) e `Web.offline`/`vagalumeKey`/`location`/`refreshMinutes` = `Config.offline`/`vagalumeKey`/`weatherLocation`/`weatherRefresh`.
  - `MediaState`: sai o `trackKey`/`onTrackKeyChanged`; `lyricsEnabled` fica; `lyricsLoading` = `Lyrics.loading`; `readonly property string lyricsNote` (com linhas, `""`; senão, nesta ordem: `Config.offline` → "Modo offline: a letra não é buscada"; sem `Web.available` → "Indisponível: sem conexão com o lucernad." ou `` `Indisponível: ${Web.error}` ``; `searching` → "Procurando a letra…"; `error` → o `Lyrics.error`; senão "Sem letra"). `MediaPage`: o aviso usa `lyricsNote`; a letra de texto (Vagalume) rola livre, como a sem tempo de hoje, **sem rótulo da fonte** (os créditos ficam na página Créditos).
  - `tests/coverage.json`: `"services/Web.qml": ["wantedTopics", "sync", "configureParams", "configure", "apply", "applyStatus", "connectionChanged", "refresh", "searchCity"]`; `"services/Lyrics.qml": ["wantedTopics", "sync", "trackParams", "sendTrack", "toLine", "apply", "applyLyrics", "clear", "connectionChanged", "lineAt", "lineRole"]`.

- [ ] **Passo 1: os testes** (cabeçalho dizendo o que testam; guardam e restauram tudo o que mudam; `wired()`, `ok`, `ids`, `methods`, `last` como no `tests/clipboard.test.js`, com o hello oferecendo `web.lyrics`, `web.weather` e `web.status`).
  - `tests/web.test.js`:
    - `web: tópicos e configuração na conexão`: `wantedTopics()` = `["web.status"]`; com `offline` verdadeiro, `vagalumeKey` `""`, `location` `null` e `refreshMinutes` 30 postos pelo teste (o `DashboardState` pode ter ligado outros) e `wired()`, um `lucerna.subscribe` pendente com `web.status` e um `web.configure` com `{offline: true, vagalumeKey: null, location: null, refresh: 30}`.
    - `web: a configuração muda e vai de novo`: `wired()`; `location` posto em `{name: "Cacaulândia, RO, BR", latitude: -10.3, longitude: -62.9}`, `vagalumeKey` `"abc"` e `offline` falso → o último `web.configure` com os três; `vagalumeKey` `""` → `null`; `refreshMinutes` 2 e 2000 → `refresh: null`, 60 → 60; `refresh()` → um `web.refresh` pendente (a chamada do `refresh` na cobertura).
    - `web: estado pelo sinal real`: `client.received("web.status", JSON.parse(JSON.stringify({available: true, error: null, online: true})))` → `available` e `online` verdadeiros; `{available: false, error: "x", online: false}` → `available` falso; `connectionChanged(false)` → `online` falso.
    - `web: busca de cidade pela resposta real`: `searchCity("Cacau", r => got = r)` → um `web.searchCity` pendente com `{query: "Cacau"}`; o `client.receive(ok(id, {results: [...]}))` → `got.result.results.length` 2; desconectado → o `done` recebe o erro na hora.
    - `web: null e tópico desconhecido não mudam nada`; `web: estado original restaurado`.
  - `tests/lyrics.test.js`:
    - `lyrics: tópicos conforme a aba`: `wantedTopics(false)` = `[]`, `(true)` = `["web.lyrics"]`; com `wired()`, ligar `active` põe a assinatura pendente.
    - `lyrics: a faixa vai pelo fio`: `trackParams("T", "A", "Al", 201.4)` = `{title: "T", artist: "A", album: "Al", duration: 201.4}`; com 0 → `duration: null`; `wired()` → `sendTrack()` deixa um `web.track` pendente.
    - `lyrics: o configure vai antes da faixa na conexão`: com o `wired()` (que conecta o `Web` e o `Lyrics` ao mesmo cliente de teste), na lista de métodos pendentes o `web.configure` vem antes do primeiro `web.track`.
    - `lyrics: a letra sincronizada pelo sinal real`: `client.received("web.lyrics", JSON.parse(JSON.stringify({key: "k", state: "found", synced: true, lines: [{time: 1, text: "a"}, {time: 3, text: ""}], source: "lrclib"})))` → `lines` `[{time: 1, text: "a"}, {time: 3, text: ""}]`, `synced`, `loading` falso; `lineAt(0.5)`, `lineAt(1)`, `lineAt(60)` = −1, 0, 1.
    - `lyrics: o Vagalume, sem tempo e com o link`: `{state: "found", synced: false, lines: [{time: null, text: "a"}], source: "vagalume", url: "https://www.vagalume.com.br/x.html"}` → `lines[0].time` −1, `lineAt(10)` −1, `lineRole(0, -1, false)` `"plain"`, `url` igual; com `url: "javascript:x"` → `url` `""`.
    - `lyrics: procurando, sem letra e erro`: `searching` → `loading` verdadeiro e `lines` `[]`; `none` → `loading` falso; `error` com `"e"` → `error` `"e"`; `connectionChanged(false)` → tudo limpo.
    - `lyrics: o aviso da aba Mídia` (pelo `t.MediaState`): `Config.offline` verdadeiro → "Modo offline: a letra não é buscada"; desconectado → "Indisponível: sem conexão com o lucernad."; `wired()` com `searching` → "Procurando a letra…"; `none` → "Sem letra".
    - `lyrics: null e tópico desconhecido não mudam nada`; `lyrics: estado original restaurado`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces (com o `get MediaState()` no `runner.qml` se ainda não houver).
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE web casos=6`, `SUITE lyrics casos=9`; `grep -rn "XMLHttpRequest\|lrclib.net" services features` → só o `Weather.qml` (sai na Tarefa 10).
- [ ] **Passo 5: commit.** `git commit -m "Rede: a letra vem do lucernad; sai o XMLHttpRequest do Lyrics"`

---

### Tarefa 10: `lucerna-shell` — `Weather.qml` como adaptador, a busca de cidade e a chave nas Configurações

**Files:**
- Modify: `services/Weather.qml` (reescrito), `tests/weather.test.js` (reescrito), `features/dashboard/state/{DashboardState,OverviewState}.qml`, `features/settings/state/SettingsState.qml:204-255`, `features/settings/ui/DashboardPage.qml:140-335`, `tests/runner.qml` (`get SettingsState()`, se ainda não houver, e a `weatherSample()` exposta no `t`), `tests/coverage.json`

**Interfaces:**
- Consumes: `Lucernad.client`; `Web` (Tarefa 9: `available`, `refresh`, `searchCity`, `refreshMinutes`); o Formato.
- Produces:
  - `Weather` (dono `"weather"`): `property var client: Lucernad.client`; `property bool active: false`; `property var current: null` (`{temp, feels, humidity, wind, code, isDay}`); `property var daily: []` (`[{date: "AAAA-MM-DD", max, min, code, rainChance}]`, até 7); `property string sunrise: ""`, `property string sunset: ""` (`"HH:MM"`); `property var updatedAt: null` (`Date`); `property string state: ""`; `property string error: ""`; `property var location: null` (a do tópico); `property var results: []` (`[{name, label, latitude, longitude}]`); `property bool searching: false`; `property string searchError: ""`; `property string lastQuery: ""`; `readonly property bool loading: state === "loading"`; `signal located(var location)`; funções `wantedTopics(active: bool): var` (`["web.weather"]` com `active`, `[]` sem), `sync()`, `apply(topic, data)`, `applyWeather(data: var): void` (`Array.from(data.daily ?? [])`; `updatedAt` = `new Date(updatedAt * 1000)` ou `null`), `toCurrent(c: var): var`, `toDay(d: var): var`, `clock(iso: string): string` (`"2026-10-24T05:41"` → `"05:41"`; inválido → `""`), `clear(): void`, `connectionChanged(connected: bool): void` (caída: `clear()`, `sync()`), `search(name: string): void` (vazio não sai; `lastQuery` = o texto; `Web.searchCity` com o `searchDone`), `searchDone(query: string, reply: var): void` (resposta de uma busca que não é a última → ignorada; erro → `searchError` = `reply.error.message`, `results` `[]`; sem resultado → `searchError` = `` `Não encontrei "${query}"` ``), `toResult(r: var): var` (`name` = `[r.name, r.state, r.countryCode]` sem vazios, juntados por `", "`; `label` = `[r.name, r.state, r.country]` sem vazios, por `" · "`), `choose(index: int): string` (emite `located({name, latitude, longitude})`, limpa `results`, devolve o `name`; índice fora → `""`), `refresh(): void` (`Web.refresh()`), `isStale(): bool` (sem `updatedAt` ou mais velho que `Web.refreshMinutes`); `onActiveChanged: sync()`; o `Connections` no `client`; o `IpcHandler` `weather` (`search(query: string): void`, `choose(index: int): string`, `refresh(): void`). Saem `online`, `refreshInterval`, `hourly`, `get`, o `Timer` e o `XMLHttpRequest`.
  - `DashboardState`: saem os `Binding` do `Weather.online`, `Weather.refreshInterval` e `Weather.location` (o `Weather` novo não tem esses campos); `Weather.active` como hoje; o `Connections` do `located` → `Config.weatherLocation` fica.
  - `OverviewState`: `weatherText` = `Config.offline` → "Modo offline"; sem `Web.available` → "Indisponível"; com dado → a descrição; com `Config.weatherLocation` → "Carregando…"; senão "Escolha a cidade nas configurações"; `weatherTemp` com `Weather.current.temp`.
  - `SettingsState`: `cityResults` (`Weather.results`), `citySearching`, `citySearchError`, `searchCity(name)`, `chooseCity(index: int): void`, `refreshWeather(): void`, `vagalumeKey` (`Config.vagalumeKey`), `setVagalumeKey(key: string): string` (aparado; vazio ou só `[A-Za-z0-9_-]{1,128}` → grava e devolve `""`; senão não grava e devolve "A chave só tem letras, números, - e _ (até 128)."), `webNote` (`""` com `Web.available`; senão o "Indisponível…" do padrão); saem `weatherLoading`/`weatherError`.
  - `DashboardPage`: na seção do clima, o campo e o botão "Buscar" de hoje, a lista dos resultados (cada linha clicável com o `label`, chama `chooseCity`), o erro, e um `TonalButton` "Atualizar agora" (`refreshWeather`, desabilitado offline); na seção "Mídia", a linha "Chave do Vagalume" (`TextInput` com `echoMode: TextInput.Password`, o erro do `setVagalumeKey` embaixo, e o link "Criar uma chave" para a página da sonda (c) ou a que a Tarefa 4 confirmar); textos: a letra "Busca a letra na LRCLIB (e no Vagalume, com a sua chave), que recebem o título, o artista, o álbum e a duração da faixa"; o modo offline "Desliga tudo o que usa a internet: o clima (Open-Meteo) e as letras (LRCLIB e Vagalume). O resto do shell não sai da máquina".
  - `tests/coverage.json`: `"services/Weather.qml": ["wantedTopics", "sync", "apply", "applyWeather", "toCurrent", "toDay", "clock", "clear", "connectionChanged", "search", "searchDone", "toResult", "choose", "refresh", "isStale"]`.

- [ ] **Passo 1: os testes** (`tests/weather.test.js`; guarda e restaura o que muda; `wired()` com os três `web.*`; `t.weatherSample()` = o exemplo do Formato com 7 dias, uma função do `tests/runner.qml` exposta no `t`, a mesma que a Tarefa 11 usa: o `ci/lib.sh` só copia o `runner.qml` e os `*.test.js`, então um arquivo de ajudantes à parte não serve).
  - `weather: tópicos conforme quem mostra`: `wantedTopics(false)`/`(true)`; ligar `active` com `wired()` põe a assinatura pendente; `refresh()` → um `web.refresh` pendente (a chamada do `refresh` na cobertura).
  - `weather: a previsão pelo sinal real`: `client.received("web.weather", JSON.parse(JSON.stringify(t.weatherSample())))` → `current` `{temp: 31.2, feels: 35, humidity: 58, wind: 7.6, code: 2, isDay: true}`, `daily.length` 7, `daily[0]` `{date: "2026-10-24", max: 33.4, min: 22.1, code: 95, rainChance: 80}`, `sunrise` `"05:41"`, `sunset` `"18:03"`, `updatedAt.getTime()` = 1792847400000, `state` `"ready"`.
  - `weather: sem cidade, carregando e erro com dados`: `{state: "none", current: null, daily: []}` → `current` `null`; `{state: "error", error: "x", current: {...}}` → `error` `"x"` e `current` mantido; `clear()` → `current` `null`, `daily` `[]`, `state` `""` (a chamada do `clear` na cobertura).
  - `weather: a busca de cidade pela resposta real`: `wired()`; `search("Cacau")` → `web.searchCity` pendente; a resposta com 3 cidades pelo `client.receive` → `results[0]` `{name: "Cacaulândia, Rondônia, BR", label: "Cacaulândia · Rondônia · Brasil", latitude: -10.3375, longitude: -62.8986}`; uma resposta de uma busca anterior chegando depois → ignorada; a resposta `{results: []}` → `searchError` `Não encontrei "Cacau"`; o erro `1001` → `searchError` a mensagem.
  - `weather: escolher salva a cidade`: com os resultados, um `Connections` no `located` conta; `choose(0)` → `"Cacaulândia, Rondônia, BR"`, o sinal com `{name, latitude, longitude}` e `results` `[]`; `choose(5)` → `""`.
  - `weather: previsão velha`: `updatedAt` `null` → `isStale()`; de agora → não; de 2 h atrás com `refreshMinutes` 30 → sim.
  - `weather: relógio do nascer e do pôr`: `clock("2026-10-24T05:41")` `"05:41"`; `clock("")` e `clock("x")` → `""`.
  - `weather: a chave nas configurações` (pelo `t.SettingsState`): `setVagalumeKey(" abc-1_2 ")` → `""` e `Config.vagalumeKey` `"abc-1_2"`; `setVagalumeKey("tem espaço")` → o texto de erro e a chave anterior fica; `setVagalumeKey("")` → limpa.
  - `weather: null e tópico desconhecido não mudam nada`; `weather: estado original restaurado`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces (a `WeatherPage` de hoje continua compilando: se ela usa o que saiu (`hourly`, `search`), troque por vazio até a Tarefa 11).
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE weather casos=10`; `grep -rn "XMLHttpRequest\|open-meteo.com/v1\|lrclib.net/api" services features` → nada.
- [ ] **Passo 5: commit.** `git commit -m "Rede: o clima e a busca de cidade vêm do lucernad; a chave do Vagalume nas configurações"`

---

### Tarefa 11: `lucerna-shell` — a aba Clima nova

**Files:**
- Modify: `features/dashboard/ui/WeatherPage.qml` (reescrita), `features/dashboard/state/WeatherState.qml`, `core/widgets/Icons.qml` (`sunrise`, `sunset`), `README.md:100,152`, `tests/runner.qml` (`get WeatherState()`)
- Create: `tests/weatherstate.test.js`

**Interfaces:**
- Consumes: `Weather` (Tarefa 10); `Web` (Tarefa 9); a referência `~/Documentos/projetos/caelestia-lab/shell/modules/dashboard/WeatherTab.qml`.
- Produces:
  - `WeatherState`: `available` (`Web.available`), `hasLocation` (`Config.weatherLocation !== null`), `city` (o nome até a primeira vírgula), `today` (a data de hoje por extenso, `Format.capitalize`, ex.: "Sábado, 24 de outubro"), `sunrise`/`sunset` (`Weather.*` ou `"--:--"`), `icon`, `temp`, `condition`, `humidity`, `feels` (só a temperatura), `wind`, `property string todayKey` (a data local de hoje, `AAAA-MM-DD`, de `new Date()`; um `Timer` de 60 s a mantém certa depois da meia-noite; os testes a põem à mão), `days` (`[{label, date, icon, range}]`: `dayLabel(d.date, todayKey)`, `shortDate(d.date)`, o ícone do dia, `` `${min} / ${max}` ``), `notice` (nesta ordem: indisponível → "Indisponível: sem conexão com o lucernad." ou `` `Indisponível: ${Web.error}` ``; `Config.offline` → "Modo offline: sem atualização"; sem `Web.online` → "Sem internet: mostrando a última previsão"; `Weather.state === "error"` → o `Weather.error`; senão `""`), `updatedNote` (`""` com a previsão nova; com `isStale()` ou `error` e `updatedAt` → `` `Atualizado às ${HH:mm}` ``); funções `temperature`, `speed`, `describe` (as de hoje), `localDate(date: string): var` (`AAAA-MM-DD` → `new Date(y, m - 1, d)`, a meia-noite **local**; nunca `new Date("AAAA-MM-DD")`, que é meia-noite UTC e cai no dia anterior a oeste de Greenwich), `dayLabel(date: string, today: string): string` (`"Hoje"` quando `date === today`, pela data e nunca pelo índice; senão o dia da semana abreviado sem ponto, capitalizado: "Dom"), `shortDate(date: string): string` (`"25 out"`), `openSettings(): void` (`Config.settingsTopic = "dashboard"`; `Panels.open("settings")`). Saem `hourly`, `search`, `loading`, `place`, `rainChance`, `weekMin`/`weekMax`, `daily`.
  - `WeatherPage` (o `implicitHeight` pelo layout): sem cidade, o `EmptyState` com `Icons.location`, "Escolha a cidade nas configurações" e o botão "Abrir configurações" (`openSettings`); sem o módulo, o `EmptyState` com o `notice`. Com cidade, a coluna do Caelestia com os componentes do Lucerna (`Surface`, `Txt`, `Icon`, `ThemeManager.spacing/font/radius/colors`): (1) a linha do topo: à esquerda `city` (`font.large + 8`, `DemiBold`) e `today` (`font.small`, `textMuted`); à direita, dois pares ícone + rótulo/valor ("Nascer do sol" `sunrise`, "Pôr do sol" `sunset`), ícones em `accent`; (2) o card grande (`Surface`, raio `radius.large * 2`): ícone do tempo grande (~3× o `font.huge`/2), `temp` em `font.huge + 12`, `condition` em `font.normal`, `textMuted`; (3) três cards iguais lado a lado, altura 60: `Icons.humidity` "Umidade", `Icons.temperature` "Sensação", `Icons.wind` "Vento" (ícone à esquerda, rótulo `font.small` com opacidade 0,7, valor `DemiBold`); (4) "Previsão de 7 dias" (`DemiBold`) e sete cards (`label` em `accent` `DemiBold`, `date` `font.small` 0,7, ícone, `range` `font.small` `DemiBold`); (5) o rodapé discreto (Decisões): à esquerda o `notice` (ou o `updatedNote`), só quando não vazio; sem crédito (sai o "Previsão da Open-Meteo" de hoje). **Sem campo de busca nem botão de atualizar na aba.**
  - `Icons`: `readonly property string sunrise: "wb_twilight"`, `sunset: "bedtime"`.
  - `README.md`: a linha 100 sem a faixa por hora e com "clima (Open-Meteo, pelo `lucernad`)"; a 152 com o Vagalume (só com a chave do usuário) e o `lucernad` fazendo os pedidos (a seção "## Créditos" é da Tarefa 11a).
- [ ] **Passo 1: os testes** (`tests/weatherstate.test.js`, pelo `t.WeatherState`, com o `Weather` posto à mão e restaurado).
  - `clima: as datas são locais` (a unidade roda com `TZ=America/Sao_Paulo`, UTC−3, fixo no `ci/lib.sh`: o `new Date("2026-10-25")` daria 24): `localDate("2026-10-25").getDate()` 25 e `getHours()` 0; `shortDate("2026-10-25")` `"25 out"`; `shortDate("2026-01-01")` `"1 jan"`.
  - `clima: os rótulos dos dias pela data`: `dayLabel("2026-10-24", "2026-10-24")` `"Hoje"`; `dayLabel("2026-10-25", "2026-10-24")` `"Dom"`; `dayLabel("2026-10-23", "2026-10-24")` `"Sex"` (uma previsão de ontem guardada nunca vira "Hoje").
  - `clima: os cards`: com o exemplo do Formato (o `t.weatherSample()` da Tarefa 10) aplicado pelo `client.received` e `todayKey` posto em `"2026-10-24"`, `days.length` 7, `days[0].label` `"Hoje"`, `days[0].range` `"22° / 33°"`; `humidity` `"58%"`, `feels` `"35°"`, `wind` `"8 km/h"`; `sunrise` `"05:41"`.
  - `clima: o aviso`: desconectado → "Indisponível: sem conexão com o lucernad."; `Config.offline` verdadeiro (com `wired()`) → "Modo offline: sem atualização"; `updatedNote` com `updatedAt` de 2 h atrás → `"Atualizado às HH:mm"` daquela hora.
  - `clima: estado original restaurado`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE weatherstate casos=5`. **A aba no container:** `dev/test.sh up`; `dev/test.sh setc 'dict(weatherLocation={"name":"Cacaulândia, Rondônia, BR","latitude":-10.3375,"longitude":-62.8986})'`; com o `lucernad` da `feat/web` copiado e reiniciado como na Tarefa 8, passo 6 (sem `LUCERNA_TEST_HOST_BUS`); `dev/test.sh see clima dashboard open weather` → leia a captura e confira, item por item, a lista do `WeatherPage` acima contra o `WeatherTab.qml` do Caelestia (e a tela larga e estreita: o painel tem até 920 px). Com o daemon parado, `dev/test.sh see clima-fora dashboard open weather` → "Indisponível". Ajustes visuais até bater; `dev/test.sh errs` sem avisos.
- [ ] **Passo 5: commit.** `git commit -m "Rede: a aba Clima no desenho do Caelestia"`

---

### Tarefa 11a: `lucerna-shell` — a página Créditos

Não depende do daemon: pode andar junto das Tarefas 9–10.

**Files:**
- Create: `features/settings/ui/CreditsPage.qml`, `tests/credits.test.js`
- Modify: `features/settings/state/SettingsState.qml:29-47` (o tópico e os dados), `features/settings/ui/Settings.qml:295-313,455` (o `credits: creditsPage` no mapa e o `Component`), `features/settings/ui/AboutPage.qml:79-82` (sai a linha "Serviços na internet"), `core/widgets/Icons.qml` (`credits`), `README.md:319-321` (a seção "## Créditos"), `ci/smoke.sh:443-444` (`credits` antes de `about` na volta pelos tópicos), `tests/runner.qml` (a suíte `credits` e o `get SettingsState()`, se ainda não houver)

**Interfaces:**
- Consumes: o padrão das páginas das Configurações (`SettingSection`, `SettingRow`, o mapa `id → Component` do `Settings.qml`, a lista `topics` do `SettingsState`).
- Produces:
  - `SettingsState.topics` ganha `{ id: "credits", icon: Icons.credits, label: "Créditos", description: "Inspiração, dados e base" }`, logo antes do `about`; `readonly property var credits` com três seções, cada item `{ name, detail, url }` (URL `https://` sempre):
    - "Inspiração": Caelestia Shell (`https://github.com/caelestia-dots/shell`; "O painel superior, a aba Clima e as curvas de animação do Material 3; GPL-3.0").
    - "Dados": Open-Meteo (`https://open-meteo.com/`; "Previsão do tempo e busca de cidade; dados sob a licença CC BY 4.0"), LRCLIB (`https://lrclib.net/`; "Letras sincronizadas"), Vagalume (`https://www.vagalume.com.br/`; "Letras, só com a chave de quem usa").
    - "Base": Quickshell (`https://quickshell.org/`), Hyprland (`https://hyprland.org/`), Material Symbols (`https://fonts.google.com/icons`; "A fonte dos ícones, do Google; Apache 2.0").
  - `SettingsState.openLink(url: string): void` (`Qt.openUrlExternally`, só `https://`).
  - `CreditsPage`: uma `SettingSection` por seção, uma `SettingRow` por item (`title` = `name`, `description` = `detail`), com um `IconButton` de link (`Icons` de abrir fora) que chama `openLink(url)`.
  - `Icons.credits` = `"volunteer_activism"`.
  - `AboutPage`: sai o `SettingRow` "Serviços na internet" e nada entra no lugar (nem Caelestia nem Vagalume).
  - `README.md`, "## Créditos": o mesmo conteúdo da página, em três listas com link em cada item (o texto de hoje sobre o Caelestia fica, acrescido da aba Clima), e o aviso do Vagalume: os termos da API pedem crédito com link junto do conteúdo e dizem que a API não transfere o direito de exibir a letra; o Lucerna é de uso próprio, a chave é de cada usuário e usar o Vagalume é escolha dele; **deixar claro que o crédito existe de forma indireta**: para usar o Vagalume, a pessoa precisa obrigatoriamente entrar no site deles e gerar a própria chave (`https://auth.vagalume.com.br/settings/api/`), e o Lucerna não traz chave nenhuma; se o Vagalume pedir, o rótulo com link junto da letra é acrescentado (o `lucernad` já manda a fonte e a página).
- [ ] **Passo 1: os testes** (`tests/credits.test.js`, pelo `t.SettingsState`).
  - `créditos: o tópico vem antes do Sobre`: o índice de `"credits"` em `topics` é o do `"about"` menos 1.
  - `créditos: as três seções e os links`: os títulos `["Inspiração", "Dados", "Base"]`; os nomes `Caelestia Shell`, `Open-Meteo`, `LRCLIB`, `Vagalume`, `Quickshell`, `Hyprland`, `Material Symbols` presentes; todo `url` começa por `https://`; o `detail` do Open-Meteo contém "CC BY 4.0".
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE credits casos=2` (o `rules` confere os links do README); `grep -n "Serviços na internet\|Caelestia\|Vagalume" features/settings/ui/AboutPage.qml` → nada; no container, `dev/test.sh see creditos settings open credits` → a página com as três seções (leia a captura).
- [ ] **Passo 5: commit.** `git commit -m "Créditos: uma página nas Configurações; sai a linha da tela Sobre"`

---

### Tarefa 12: `lucerna-shell` — o `lucernad` novo na fumaça, PR e merge

**Files:**
- Create: `ci/web-fake.py`
- Modify: `dev/lucernad.rev` (`<LUCERNAD_REV>`), `ci/smoke.sh`, `ci/packages/smoke.txt` (`ffmpeg`, explícito), `ci/tolerated.txt` (só o que for medido), `Lucerna — Pendências.md:38` (vira `[x]` com "(feito em 2026-10-24: módulo `web` do `lucernad`)"), `Lucerna — Proposta.md:127-128,207-208` (os serviços como adaptadores do `lucernad`; uma linha do Vagalume: "Letras, só com a chave do usuário | Título e artista da faixa")

**Interfaces:**
- Consumes: `<LUCERNAD_REV>` (Tarefa 8); `lucernad --json status`; o IPC `weather` (Tarefa 10).
- Produces:
  - `ci/web-fake.py <arquivo-do-porto>` (só a biblioteca padrão; `http.server` num `ThreadingHTTPServer` em `127.0.0.1:0`; grava o porto no arquivo; um log de cada pedido em `stderr`): `GET /lrclib/api/get` com `track_name` "Canção da Fumaça" e `artist_name` "Lucerna" → 200 com `syncedLyrics` de quatro linhas a cada 5 s (`[00:00.00]` … `[00:15.00]`); o resto → 404 `TrackNotFound`; `/lrclib/api/search` → `[]`; `/forecast/v1/forecast` → a previsão de 7 dias do Formato (datas a partir de hoje); `/geocoding/v1/search` com `name` começando por "Cacau" → três cidades (Cacaulândia/RO primeiro), o resto → `{}`; qualquer outro caminho → 404. Roda com `python3 -I`.
  - `ci/smoke.sh`: antes do `mpv`, `ffmpeg -loglevel error -f lavfi -i anoisesrc=color=pink:amplitude=0.5 -t 20 -metadata title="Canção da Fumaça" -metadata artist="Lucerna" "$work/fumaca.ogg"` e o `mpv` toca esse arquivo (`--loop-file=inf`, o resto igual); o `web-fake.py` sobe no próprio grupo antes do `lucernad`, e o `lucernad serve` ganha `LUCERNAD_WEB_BASE=http://127.0.0.1:<porto>`; reprova sem o aviso "a rede é de teste" no `lucernad.log`. A fase **"rede"**, depois da fase "brilho": `ipc call weather search Cacau`; em 5 s, `ipc call weather choose 0` devolve "Cacaulândia, Rondônia, BR" (`row "rede" "cidade escolhida em 5 s" …`); em 5 s, o `lucernad --json status` com `web.weather` `state` `ready` e 7 dias (`row … "previsão em 5 s"`); `dashboard open weather` e a captura `NN-clima.png`; a aba Mídia aberta e, em 5 s, `web.lyrics` `found` com `source` `lrclib` e 4 linhas (`row … "letra em 5 s"`) e a captura `NN-letra.png`; o log do `web-fake.py` com pelo menos um pedido de cada fonte. Na fase "lucernad fora", a aba Clima capturada com "Indisponível". O cabeçalho do script ganha a fase "rede", o servidor falso e as reprovações dela; o `web-fake.py` entra na lista do que a fumaça encerra por PID.
- [ ] **Passo 1: o servidor falso e o player com etiquetas.** Verificar: `python3 -I ci/web-fake.py /tmp/porto & curl -s "http://127.0.0.1:$(cat /tmp/porto)/geocoding/v1/search?name=Cacau"` → as três cidades; `ffmpeg … && mpv --no-video --frames=1 …` mostra o título; `ruff check ci/web-fake.py` limpo (o `lint` do CI).
- [ ] **Passo 2: a fumaça.** `smoke.sh`, `packages/smoke.txt`, `tolerated.txt`. Verificar: `dev/ci.sh smoke` → verde, com as linhas da fase "rede" no `ci-out/smoke/summary.md` e as capturas do clima, da letra e da aba Clima indisponível (leia as três); `dev/ci.sh` (os quatro) → verde.
- [ ] **Passo 3: commit, PR e merge.** `git commit -m "Rede: o lucernad novo na fumaça, com o servidor falso de letras e clima"`; `git push -u origin feat/rede-externa`; `gh pr create --repo lucerna-desktop/lucerna-shell --base main --title "Rede externa: a letra e o clima vêm do lucernad; a aba Clima nova" --body <resumo; a revisão fixa do lucernad; como foi testado (unidade pelo fio do cliente e pelo sinal real, a fumaça com o servidor falso, a conferência da Tarefa 8); as capturas da aba Clima; a chave do Vagalume na configuração; a página Créditos e o aviso do Vagalume no README>`. **Controlador:** revisão final da branch (Opus), CI verde, `gh pr merge <n> --repo lucerna-desktop/lucerna-shell --merge`.

---

## Autorrevisão

**Cobertura do spec:** cliente HTTP próprio com prazo de 10 s, `User-Agent` com o link, nova tentativa em prazo/5xx/429 com `Retry-After`, no máximo 3 (Tarefa 2); modo offline sem nenhum pedido (Tarefas 6 e 8); conectividade do NM, sem tentar com `none`, tentando sem NM (Tarefas 5 e 6); ordem LRCLIB `get` → `search` → Vagalume só com chave (Tarefa 4); casamento normalizado com sufixos, acentos e ±2 s, preferindo sincronizada, "na dúvida, nenhuma" (Tarefas 1 e 4); LRC completo com várias marcas, `offset` e pausas (Tarefa 1); Vagalume com a chave do usuário e `source`/`url` no tópico, sem rótulo nos painéis (Tarefas 4, 6, 9); cache em disco pela `lucerna-paths`, letra sem prazo, "sem letra" 7 dias, falha nunca guardada, limite de 50 MB (Tarefas 3, 4, 6); busca só com assinante, `web.track` a cada troca, a faixa atual ao ganhar assinante (Tarefas 6 e 9); sem bug de carregando (Tarefas 6 e 9); clima com `sunrise`/`sunset` e 7 dias, sem `hourly`, o guardado publicado na hora e a busca no fundo quando velho, só com assinante (Tarefas 4, 6, 10); busca de cidade com até 5 resultados em pt (Tarefas 4, 6, 10); os três tópicos e os quatro métodos com `-32602` em pt-BR e a chave fora de log e tópico (Formato, Tarefas 2, 6, 7); os adaptadores com os donos `lyrics` e `weather`, sem `XMLHttpRequest` (Tarefas 9–10); a letra do Vagalume rolável e sem rótulo da fonte (Tarefa 9); a chave e a busca com lista nas Configurações, "Atualizar agora" nas Configurações e no IPC (Tarefa 10); a aba Clima nova sem busca nem botões, com o aviso e o botão das Configurações sem cidade, a hora da última atualização quando velha e o aviso offline (Tarefa 11); os créditos num lugar só: a página Créditos, a seção do README com o aviso do Vagalume, nada nos painéis nem na tela Sobre (Tarefas 9, 11 e 11a); "Indisponível" sem o `lucernad` (Tarefas 9–12); os testes Rust sem internet, a máquina real com faixas do Spotify, o clima reiniciado, a busca com vários resultados e o offline, o container com captura, a fumaça sem avisos e a cobertura (Tarefas 1–12). Sem lacunas.

**Passos:** cada passo de código traz as assinaturas, os textos e os números; o único algoritmo escrito é o laço, que as assinaturas não determinam; o desenho da aba vem como a lista de blocos do Caelestia com os tokens do Lucerna.

**Tipos:** `Line`, `parse_lrc`, `plain_lines`, `lines_json`, `Track`, `normalize`, `title_key`, `first_artist`, `same_track` (Tarefa 1); `Request`, `Response`, `HttpError`, `Http`, `ReqwestHttp`, `RetryPolicy`, `fetch`, `retry_after`, `Endpoints`, `base_from_env`, `LocalServer`, `Seen`, `Reply`, `FakeHttp` (Tarefa 2); `Cache`, `Kind` (Tarefa 3); `Source`, `Lyrics`, `Found`, `fresh`, `find`, `Location`, `Current`, `Day`, `Forecast`, `City`, `HttpOrFormat`, `forecast`, `search_cities` (Tarefa 4); `Link`, `link_from`, `Connectivity`, `NmConnectivity`, `FakeLink`, `isolated` (Tarefa 5); `WebConfig`, `WebTopics`, `register`, `Web`, `fake_config`, `quiet_config(dir, base)` (Tarefas 6–7) batem entre si; no shell, `Web.available/online/searchCity/refresh/refreshMinutes` (Tarefa 9) são os que o `Weather`, o `WeatherState`, o `MediaState` e o `SettingsState` usam (Tarefas 10–11), e o `Weather.current`/`daily` mantêm `temp`/`feels`/`humidity`/`wind`/`code`/`isDay`/`max`/`min`/`rainChance`, que o `OverviewState` já usa.

**Review Focus:** os cinco itens têm teste na tarefa dona (2, 4 e 6); o item 1 também na conferência (o `grep` da chave na Tarefa 8, passo 4) e o 4 também no `ss` da Tarefa 8, passo 5.

**Proporção:** maior que o spec porque cobre dois repositórios, a fumaça e a conferência; o código fica nas assinaturas, e os blocos de código são o laço, o formato e os comandos que precisam ser exatos.
