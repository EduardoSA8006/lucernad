<p align="center"><img src="docs/assets/logo.svg" alt="Lucerna" height="120"></p>

# lucernad

Daemon em Rust com a lógica do shell Lucerna (espectro de áudio, sistema, área de transferência, energia e mais); o shell fica só com a interface.

**Situação:** três módulos prontos: o `system` (CPU, temperatura, memória, swap, rede, tempo ligado, máquina, discos e GPUs), o `spectrum` (o espectro do som do player exibido, pelo PipeWire) e o `clipboard` (o histórico da área de transferência do Wayland).

## Módulos

### `system`

Lê tudo do `/proc` e do `/sys`, sem nenhum processo externo, e só mede enquanto alguém assina `system.stats`, `system.disks` ou `system.gpus`. Os campos têm os nomes do `SystemStats.qml` do shell.

| Tópico | O que traz | Quando muda |
| --- | --- | --- |
| `system.info` | `cpuModel`, `cpuThreads`, `hostname`, `osName`, `kernel` | fixo, lido ao subir |
| `system.stats` | `cpuUsage` (0 a 1), `cpuTemp` (°C ou `null`), `memTotal`, `memUsed`, `swapTotal`, `swapUsed`, `rxRate`, `txRate` (bytes/s), `rxTotal`, `txTotal`, `rxHistory`, `txHistory` (até 30 taxas), `uptime` (s) | a cada amostra; `null` até a primeira |
| `system.disks` | `{"disks":[{"name","size","used","root"}]}`, em bytes, o da raiz primeiro | quando o valor muda; `null` até a primeira amostra |
| `system.gpus` | `{"gpus":[{"card","vendor","pci","state","name","usage","temp","freq","maxFreq"}]}`; placa suspensa não é lida (`usage`/`temp`/`freq` `null`) | quando o valor muda; `null` até a primeira amostra |
| `system.status` | `{"available":true,"error":null}`; `false` com o motivo se o módulo não puder rodar | quando o módulo para |

Método: `system.configure {"interval_ms": n}` → `{}`, com `n` inteiro de `500` a `10000` (padrão `2000`); vale o último pedido.

### `spectrum`

Captura pelo PipeWire **só o som do player exibido** (não a saída inteira), com uma FFT própria, e entrega 40 bandas suavizadas a 60 quadros por segundo. Roda **só com assinante e com player**: sem assinante de `spectrum.bands` ou sem player configurado, nem a conexão com o PipeWire fica aberta. Os fluxos de captura são só de entrada, ligados às saídas do player: o roteamento do som não muda, e quando o player some o fluxo nunca cai no microfone.

| Tópico | O que traz | Quando muda |
| --- | --- | --- |
| `spectrum.bands` | `{"bands":[40 números de 0 a 1, com até 3 casas]}`, dos graves aos agudos | a cada quadro (até 60 por segundo) enquanto captura, só quando muda; ao parar, um quadro de zeros; `null` até o primeiro quadro |
| `spectrum.status` | `{"available","capturing","player","error"}`: `available` diz se a última conexão com o PipeWire deu certo, `capturing` se há um nó do player sendo capturado, `player` o configurado e `error` o último erro (`null` depois de uma conexão boa) | quando o estado muda |

Método: `spectrum.configure {"player": "org.mpris.MediaPlayer2.<…>" | null}` → `{}`; outro valor dá `-32602`. Vale o último pedido.

Como o player (nome MPRIS) casa com os nós de saída do PipeWire:

1. O nó é do player se o PID do client dele é o do dono do nome no D-Bus ou um descendente dele; quando o dono é uma ponte conhecida (o `plasma-browser-integration-host`), a busca parte do pai.
2. Sem o dono no `/proc` (ou com o proxy do Flatpak), a reserva compara o executável do client com o nome do player (o fim do nome D-Bus, sem o `.instance…`, ou o `DesktopEntry`), só com igualdade exata e só se todos os nós que casam vierem de um mesmo processo.
3. Vários nós casando somam (as abas do navegador tocando juntas); em dúvida, nenhum: o anel parado é melhor que as barras de outro app.

### `clipboard`

Observa a área de transferência pelo protocolo de controle de dados do Wayland (`ext-data-control-v1`, com o `wlr-data-control` de reserva) e guarda o histórico em SQLite, com busca por trechos (sem distinguir maiúsculas nem acentos). Imagens (`png`, `jpeg`, `webp`) ganham miniatura. Roda **depois do primeiro `clipboard.configure` (ou 2 s sem ele, com o padrão) e só com assinante de `clipboard.history`**, ou enquanto segura a seleção que o próprio daemon pôs (um `clipboard.copy`): a conexão e o trabalho em andamento ficam até outro app copiar. Cópia que anuncia o tipo `x-kde-passwordManagerHint` (gerenciadores de senha) **nunca entra** no histórico: nem é lida.

| Tópico | O que traz | Quando muda |
| --- | --- | --- |
| `clipboard.history` | `{"revision","count","pinned"}`; `null` até o histórico abrir | a cada item novo, remoção, fixação ou limpeza |
| `clipboard.status` | `{"available","error"}`: `available` diz que a última abertura da sessão deu certo e o banco abriu; `false` com o motivo em `error` (sem sessão Wayland, compositor sem o controle, banco que não abriu); `false` com `error` `null` enquanto ainda não foi tentado | quando o estado muda |

Métodos:

- `clipboard.list {kind?, query?, offset, limit}` → `{"items":[…],"total"}`: uma página (`limit` de 1 a 200), com a prévia e os trechos marcados (`matches`, em unidades UTF-16); `kind` é `text`, `image` ou `files`.
- `clipboard.get {id}` → o item (texto até 256 KiB, com `truncated`; ou as `uris`).
- `clipboard.copy {id}` → `{}`: o daemon vira dono da seleção com o item.
- `clipboard.copyImage {path, store?}` → `{id}`: copia uma imagem do disco (com `store: false`, sem guardar no histórico; `id` `0`).
- `clipboard.remove {id}`, `clipboard.pin {id, pinned}` e `clipboard.clear {all?}` → `{}`.
- `clipboard.configure {limit, persist, waylandDisplay?}` → `{}`: o limite de itens (1 a 1000) e a persistência.

O histórico mora em `$XDG_DATA_HOME/lucerna/clipboard/` (`history.db` e `images/`); sem persistência, só em memória e as imagens em `$XDG_RUNTIME_DIR/lucerna/clipboard/images/`, apagadas ao parar. O `WAYLAND_DISPLAY` do ambiente do daemon vence o do `configure`.

## Instalação

Para compilar: o `gcc` (o SQLite do histórico é compilado junto), `libpipewire`, `clang` e `pkgconf` (Arch) ou `libpipewire-0.3-dev`, `libclang-dev` e `pkg-config` (Debian/Ubuntu). O espectro precisa do PipeWire com o WirePlumber rodando.

```sh
cargo build --release --locked -p lucernad
sudo install -Dm755 target/release/lucernad /usr/bin/lucernad
sudo install -Dm644 -t /usr/lib/systemd/user/ crates/lucernad/systemd/lucerna-lucernad.socket crates/lucernad/systemd/lucerna-lucernad.service
sudo install -Dm644 crates/lucernad/data/lucernad.json /usr/share/lucerna/apps/lucernad.json
systemctl --user daemon-reload && systemctl --user enable --now lucerna-lucernad.socket
```

O socket fica em `$XDG_RUNTIME_DIR/lucerna/lucernad.sock`; o systemd sobe o daemon na primeira conexão, e ele sai sozinho depois de 60 s sem cliente (`LUCERNA_IDLE_TIMEOUT_MS` muda).

## Linha de comando

- `lucernad status`: o estado de todos os tópicos.
- `lucernad watch system.stats`: cada mudança de um tópico, até o Ctrl+C.
- `lucernad player org.mpris.MediaPlayer2.spotify`: escolhe o player cujo som o espectro mostra (sem nome, nenhum); `lucernad watch spectrum.bands` mostra as bandas.
- `lucernad historico [busca]`: os 20 itens mais recentes da área de transferência (com busca, só os que casam).
- `lucernad copiar ID`: põe um item do histórico na área de transferência.
- `--json` (antes do subcomando): a saída em JSON, uma linha por estado.

## Desenvolvimento

Os comandos do CI (fmt, clippy, este também sem nenhum módulo e com cada módulo sozinho, e test), com `TMPDIR=/tmp` (os testes criam sockets Unix, cujo caminho tem limite de ~108 bytes):

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p lucernad --no-default-features --all-targets --locked -- -D warnings
cargo clippy -p lucernad --no-default-features --features system --all-targets --locked -- -D warnings
cargo clippy -p lucernad --no-default-features --features spectrum --all-targets --locked -- -D warnings
cargo clippy -p lucernad --no-default-features --features clipboard --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Para rodar o daemon sem o systemd: `cargo run -p lucernad -- serve`.

A sonda do backend Wayland (`crates/lucernad-clipboard/tests/sonda.rs`) só roda num compositor isolado, nunca na sessão do usuário: o `tests/isolado.sh` sobe um sway headless dentro de um container (o cabeçalho do script traz o `docker run`).

```sh
docker run --rm -v "$PWD:/src:ro" -v <toolchain>:/tc:ro archlinux:latest \
  bash -c 'pacman -Syu --noconfirm --needed sway wl-clipboard libcap gcc > /dev/null && setcap -r /usr/bin/sway \
    && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-clipboard/tests/isolado.sh'
```

## Ligação com o Lucerna

Faz parte do ecossistema do [Lucerna](https://github.com/lucerna-desktop/lucerna-shell), o shell de desktop para o Hyprland. O shell cuida só da interface; a lógica fica em Rust, neste e nos outros repositórios do ecossistema (veja [`docs/ecossistema.md`](https://github.com/lucerna-desktop/lucerna-sdk/blob/main/docs/ecossistema.md) no `lucerna-sdk`).

## Documentação

Veja a pasta [`docs/`](docs/):

- [`docs/proposta.md`](docs/proposta.md)

## Repositório

A branch `main` é protegida por um ruleset igual ao do `lucerna-shell`: PR obrigatório, conversas resolvidas, só merge commit, sem deleção nem force push. Os checks `fmt`, `clippy` e `test` são obrigatórios no ruleset.

## Licença

[GPL-3.0](LICENSE).
