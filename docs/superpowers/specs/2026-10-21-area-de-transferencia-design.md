# lucernad: o módulo da área de transferência

**Data:** 2026-10-21
**Base:** `docs/superpowers/specs/2026-10-20-sistema-design.md` (o daemon, a conexão permanente do shell, o cliente do protocolo e o "sem reserva") e `docs/superpowers/specs/2026-10-20-espectro-design.md` (o segundo módulo; o `lucernad-core`, os donos de tópicos no `LucernaClient`, o filtro pelos tópicos do `lucerna.hello`), todos implementados e mesclados.
**Repositórios:** `lucernad` (o módulo `clipboard`) e `lucerna-shell` (o serviço `Clipboard.qml` vira adaptador; sai o `wl-clipboard`). O `lucerna-sdk` não muda, salvo se o módulo provar que precisa.

## Por quê

O histórico da área de transferência do shell (painel `Super+V`) hoje roda o `wl-paste --watch` com um script que chama o `wl-paste` mais três vezes a cada cópia, manda o texto em base64 e o decodifica em JavaScript, caractere a caractere, na thread da interface (um texto de 256 KiB pode travar a tela). O histórico é um `clipboard.json` regravado inteiro a cada mudança (até ~128 MB com 500 itens grandes), com gravação síncrona e leitura que bloqueia a inicialização; uma cópia feita antes da carga descarta o histórico salvo. Só texto e `image/png` entram. O shell é só a interface: a observação, o armazenamento e a cópia de volta vão para código próprio em Rust.

## Decisões do usuário

- Pulou as perguntas e aprovou as recomendações do construtor do documento (2026-10-21), abaixo.
- **Sem migração:** o shell ainda não está em uso; o `clipboard.json` e as imagens antigas não são importados (podem ser apagados).
- **Sem reserva**, como nos outros módulos: sai o `wl-clipboard` (e o script); sem o `lucernad`, o painel mostra "Indisponível".

## Objetivos

- Observar a área de transferência do Wayland direto pelo protocolo, sem processo externo.
- Histórico em SQLite com gravação incremental, deduplicação por conteúdo, fixados e limite.
- Texto, imagens (PNG, JPEG, WebP e outras `image/*`) e arquivos copiados no gerenciador (`text/uri-list`).
- Copiar de volta pelo próprio `lucernad`, que segura a seleção mesmo se o shell recarregar.
- Busca no banco, rápida com milhares de itens, com o trecho encontrado marcado.
- O painel do `Super+V` igual ao de hoje para quem usa (mesmas teclas, fixar, apagar, filtros, limite, colar sozinho).

## Não objetivos

- A seleção primária (botão do meio).
- Sincronizar entre máquinas.
- Mudar o desenho do painel (salvo mostrar o novo tipo "arquivos" e o trecho marcado na busca).
- Migrar o histórico antigo.

## Anti-objetivos

- Trocar o `wl-clipboard` por outra ferramenta externa.
- Guardar senhas: conteúdo marcado como sensível pelo gerenciador de senhas nunca entra no histórico nem no disco.
- Travar a interface do shell com texto grande ou imagem grande.
- Perder o histórico por queda ou travamento.

## O módulo `clipboard`

### Observação

- **Protocolo de controle de dados do Wayland** (`ext-data-control-v1`, com `zwlr-data-control-unstable-v1` como alternativa se o compositor só tiver esse), por biblioteca Rust de cliente Wayland (ex.: `wayland-client` + `wayland-protocols`); escolha do construtor. O Hyprland oferece os dois; o KDE oferece o `ext`.
- **Exibição:** o `lucernad` sobe pelo systemd do usuário e precisa do `WAYLAND_DISPLAY`. Hoje ele está no ambiente do systemd (conferido em 2026-10-21 na máquina do usuário). Se faltar, o módulo fica indisponível com erro claro no `clipboard.status`, e o shell, que conhece o `WAYLAND_DISPLAY`, o informa no `clipboard.configure` (decisão do construtor do documento).
- **Conexão viva enquanto há assinante ou seleção nossa:** a observação roda enquanto houver assinante de `clipboard.history` (o shell assina sempre com o recurso ligado, então na prática roda com o shell de pé). A conexão Wayland também fica viva **enquanto o `lucernad` for dono da seleção** (depois de um `clipboard.copy`), mesmo sem assinante, para o que foi copiado não sumir quando o shell recarrega; nesse estado o módulo segura o trabalho em andamento do `lucerna-service` (`WorkTracker`), para a saída por ociosidade não derrubar a seleção. Quando outro app copia, a seleção deixa de ser nossa e, sem assinante, a conexão é solta (decisão do construtor do documento).
- **Leitura de cada cópia:** escolhe o melhor tipo oferecido, nesta ordem: imagem (`image/png`, depois outras `image/*`), lista de arquivos (`text/uri-list`), texto (`text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`, `TEXT`, `STRING`). Lê por pipe com prazo e limite de tamanho (texto até 1 MiB; imagem até 32 MiB; acima, ignora com log). Nada de HTML como item próprio.
- **Sensível:** se os tipos oferecidos incluem `x-kde-passwordManagerHint` (com valor `secret`) ou `application/x-kde-passwordManagerHint`, ignora a cópia inteira (nem lê o conteúdo).
- **Cópias do próprio `lucernad`** (a cópia de volta) não viram item novo; só sobem o item existente ao topo.

### Armazenamento

- **Banco SQLite** em `$XDG_DATA_HOME/lucerna/clipboard/history.db` (pela `lucerna-paths`), por biblioteca Rust (ex.: `rusqlite` com o SQLite embutido), modo WAL. Gravação incremental por item; uma queda não perde o que já foi gravado.
- **Item:** id, tipo (`text` | `image` | `files`), hash do conteúdo (SHA-256), texto (para `text`; para `files`, as URIs), mime e dimensões (imagem), tamanho, criado e usado por último, fixado.
- **Imagens em arquivos** `$XDG_DATA_HOME/lucerna/clipboard/images/<hash>.<ext>`, com miniatura `<hash>.thumb.png` gerada pelo `lucernad` (lado maior 272 px, o tamanho que o painel já pede). O shell só mostra os caminhos; nunca decodifica imagem grande.
- **Deduplicação por hash do conteúdo:** a mesma cópia sobe o item ao topo (mantém id e fixado), sem gravar arquivo de novo.
- **Limite:** `limit` (padrão 100; opções 25/50/100/200/500 como hoje) conta só os não fixados; ao passar, apaga os mais antigos não fixados e os arquivos deles na mesma transação.
- **Sem persistência** (`persist` desligado): o histórico fica em SQLite em memória, e as imagens e miniaturas em `$XDG_RUNTIME_DIR/lucerna/clipboard/` (tmpfs, some ao sair da sessão; o módulo apaga ao parar). Ao desligar a persistência, o que estava no disco é apagado e o histórico em memória começa vazio; ao religar, os itens em memória passam para o disco (decisão do construtor do documento).
- **Configuração antes de observar:** o módulo só começa a observar depois do primeiro `clipboard.configure` da vida do processo, para uma cópia não ir ao disco com `persist` desligado; o shell manda o `configure` antes de assinar. Sem nenhum `configure` em 2 s depois do primeiro assinante (ex.: o `lucernad watch` da linha de comando), usa o padrão (`limit` 100, `persist` ligado) (decisão do construtor do documento).
- **Limpeza:** na abertura, arquivos em `images/` sem item no banco são apagados (órfãos).

### Busca e lista

- **Busca de texto** que acha qualquer pedaço do texto, como hoje (`includes`), sem diferenciar maiúsculas e acentos, com os intervalos do trecho encontrado para o shell marcar: FTS5 com o tokenizador `trigram` (o SQLite embutido precisa ser 3.45 ou mais novo, para ignorar acentos) e, para consultas de 1 ou 2 caracteres, que o trigram não busca, uma comparação direta sobre o texto normalizado (decisão do construtor do documento).
- **Lista paginada:** o painel pede só a página que mostra (filtro por tipo, consulta, deslocamento, quantidade); fixados primeiro, depois por uso mais recente.

### Copiar de volta

- **`clipboard.copy {id}`:** o `lucernad` vira dono da seleção pelo próprio protocolo de controle de dados, oferecendo os mesmos tipos do item (imagem com o mime original; arquivos como `text/uri-list` e, se o gerenciador pedir, `x-special/gnome-copied-files`; texto como `text/plain;charset=utf-8` e equivalentes). Segura a seleção até outra cópia substituí-la, mesmo se o shell recarregar.
- **Colar sozinho** (o Ctrl+V depois de copiar) fica no shell, como hoje, porque depende do Hyprland (`send_shortcut` e a lista de terminais com Ctrl+Shift+V).

### Tópicos e métodos

- `clipboard.history` — tópico comum, `{revision, count}`: muda a cada inclusão, remoção, fixação ou limpeza (o shell repagina ao ver `revision` novo).
- `clipboard.status` — `{available, error}`: `available` diz que a última abertura da sessão deu certo e o banco abriu; `false` com `error` diz o motivo; `false` com `error` `null` quer dizer que ainda não foi tentado (antes do primeiro `configure` e do primeiro assinante ou cópia).
- `clipboard.list {kind?, query?, offset, limit}` → `{items: [{id, kind, text?, preview?, uris?, image?, thumb?, mime?, width?, height?, size, created, used, pinned, matches?}], total}`; `text`/`preview` truncados para a prévia (o texto inteiro vem por `clipboard.get {id}`).
- `clipboard.copy {id}`, `clipboard.remove {id}`, `clipboard.pin {id, pinned}`, `clipboard.clear {all}` (sem `all`, mantém os fixados) → `{}`.
- `clipboard.configure {limit, persist, waylandDisplay?}` → `{}` (faixa do limite 1–1000; `waylandDisplay` só se o ambiente não tiver).
- Erros de parâmetro com o `-32602` do SDK e texto em pt-BR, como nos outros módulos.

## Lado do shell (`lucerna-shell`)

- **`services/Clipboard.qml` vira adaptador** pelo `LucernaClient` (dono `"clipboard"`): assina `clipboard.history` e `clipboard.status` com o recurso ligado (`Config.clipboardEnabled`); manda `clipboard.configure` com `Config.clipboardLimit` e `Config.clipboardPersist` ao conectar e ao mudar.
- **`ClipboardState`/painel:** a lista vem de `clipboard.list` (paginada pela rolagem; filtro e busca no `lucernad`), mantendo as mesmas teclas, botões e filtros; o filtro ganha "Arquivos"; a busca marca o trecho; o texto inteiro vem por `clipboard.get` quando precisa.
- **`activate`:** `clipboard.copy {id}` e, com `Config.clipboardPaste`, o colar sozinho de hoje.
- **Sai do shell:** o `wl-paste --watch`, o `services/scripts/clipboard-watch.sh`, o `decode` em base64, o `clipboard.json` e o diretório de imagens no `statePath`, o `wl-copy`, e o `wl-clipboard` do README e do `dev/Dockerfile`.
- **Sem o `lucernad`** ou com o módulo indisponível: o painel mostra "Indisponível" (o `EmptyState` do `core`).
- **Teste e CI:** a fumaça exercita o painel com o `lucernad` de pé num compositor de teste (o sway da fumaça tem o protocolo de controle de dados); como copiar algo para a fumaça fica com o construtor (o próprio `lucernad` copiando, ou um cliente de teste do repositório), sem `wl-clipboard`.

## Testes

- **Rust:** a escolha do tipo, o filtro de sensível e os limites (sem Wayland, com dublê atrás de trait); o banco (inclusão, deduplicação, fixados, limite com arquivos apagados, limpeza de órfãos, sem persistência, busca FTS com acentos e trechos, paginação); a miniatura; "só com assinante"; cópia de volta servindo os tipos certos (dublê). Uma sonda num compositor Wayland isolado (sway ou weston headless em container) copia e cola de verdade.
- **Máquina real:** copiar texto, imagem (PNG e JPEG) e arquivos do gerenciador e ver no painel; senha do gerenciador de senhas não entra; colar de volta em outro app; recarregar o shell e continuar colando; histórico sobrevive a reiniciar o `lucernad`.
- **Shell:** o adaptador (assina com o recurso ligado, configura ao conectar, pagina, copia, filtro "Arquivos"), fumaça sem avisos, cobertura 100%.
- CI dos dois repositórios verde.

## O que fica para o construtor

- As bibliotecas (cliente Wayland, SQLite, imagem para a miniatura, hash).
- O esquema exato do banco e o tamanho da página.
- Como a fumaça do shell copia algo sem `wl-clipboard`.
- Os prazos de leitura do pipe e da exibição.
