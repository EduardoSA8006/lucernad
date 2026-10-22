<p align="center"><img src="docs/assets/logo.svg" alt="Lucerna" height="120"></p>

# lucernad

Daemon em Rust com a lógica do shell Lucerna (espectro de áudio, sistema, área de transferência, energia e mais); o shell fica só com a interface.

**Situação:** quatro módulos prontos: o `system` (CPU, temperatura, memória, swap, rede, tempo ligado, máquina, discos e GPUs), o `spectrum` (o espectro do som do player exibido, pelo PipeWire), o `clipboard` (o histórico da área de transferência do Wayland) e o `power` (o brilho das telas, o histórico de consumo e os perfis de energia).

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

### `power`

Ajusta o brilho das telas, lê o histórico de consumo da bateria e os perfis de energia, sem nenhum processo externo (nada de `brightnessctl`, `ddcutil` nem `busctl`). A tela integrada é lida pelo `/sys/class/backlight` e ajustada pelo logind (`SetBrightness`, sem root); os monitores externos, por DDC/CI em código próprio (o `/dev/i2c-N`, só os barramentos das placas de vídeo, falando só com o endereço 0x37 e 0x50 e gravando só o brilho). Ajustes em sequência (segurar a tecla) são juntados numa escrita só. Mudanças de fora e monitor plugado ou tirado chegam pelo uevent do kernel. Roda **só com assinante** dos tópicos (o histórico e os perfis, só com assinante deles); os métodos funcionam sozinhos, sem assinante.

| Tópico | O que traz | Quando muda |
| --- | --- | --- |
| `power.screens` | `{"screens":[{"id","kind","output"?,"name","value","max","percent","error"?}]}`: as telas com brilho ajustável (`kind` `backlight` ou `ddc`), com o valor real; `null` até a primeira descoberta | a cada ajuste, mudança de fora ou monitor plugado/tirado |
| `power.history` | `{"points":[{"time","rate","state"}]}`: o histórico de consumo do UPower (vazio sem bateria); `null` até a primeira leitura | a cada leitura (a cada minuto) |
| `power.profiles` | `{"available","performance"}`: o serviço de perfis existe e tem o perfil de desempenho; `null` até a primeira leitura | quando o serviço muda |
| `power.status` | `{"available","error"}`; `false` com o motivo se o módulo não puder rodar | quando o módulo para |

Métodos:

- `power.set {id, percent}` → `{}`: ajusta o brilho de uma tela (`percent` de 1 a 100).
- `power.change {id, delta}` → `{}`: soma `delta` ao valor atual (para as teclas de brilho).
- `power.list {}` → `{"screens":[…]}`: o mesmo do tópico, com a descoberta refeita (para a linha de comando).

Erros: `-32602` para parâmetro inválido, `1001` para tela que não existe, `1002` com o módulo indisponível e `1003` para uma tela sem barramento (a linha dela diz por quê).

O i2c só abre barramentos de placa de vídeo (classe PCI `0x03*`): SMBus, sensores e touchpad nunca são tocados. O `LUCERNAD_POWER_ROOT` é **só de teste**: aponta o módulo para uma árvore falsa (fora do `/sys`, do `/dev` e do `/proc`), sem logind nem DDC; em container, sem ele, o módulo fica indisponível.

## Instalação

Para compilar: o `gcc` (o SQLite do histórico é compilado junto), `libpipewire`, `clang` e `pkgconf` (Arch) ou `libpipewire-0.3-dev`, `libclang-dev` e `pkg-config` (Debian/Ubuntu). O espectro precisa do PipeWire com o WirePlumber rodando.

```sh
cargo build --release --locked -p lucernad
sudo install -Dm755 target/release/lucernad /usr/bin/lucernad
sudo install -Dm644 -t /usr/lib/systemd/user/ crates/lucernad/systemd/lucerna-lucernad.socket crates/lucernad/systemd/lucerna-lucernad.service
sudo install -Dm644 crates/lucernad/data/lucernad.json /usr/share/lucerna/apps/lucernad.json
sudo install -Dm644 crates/lucernad/data/60-lucernad-i2c.rules /usr/lib/udev/rules.d/60-lucernad-i2c.rules
sudo install -Dm644 crates/lucernad/data/lucernad-i2c.conf /usr/lib/modules-load.d/lucernad-i2c.conf
sudo modprobe i2c-dev
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev
systemctl --user daemon-reload && systemctl --user enable --now lucerna-lucernad.socket
```

A regra udev e o `modules-load` são só para o brilho dos monitores externos: carregam o `i2c-dev` e dão acesso ao `/dev/i2c-N` das placas de vídeo ao usuário da sessão (equivale à regra do `ddcutil`, e basta uma das duas). A tela integrada não precisa deles.

O socket fica em `$XDG_RUNTIME_DIR/lucerna/lucernad.sock`; o systemd sobe o daemon na primeira conexão, e ele sai sozinho depois de 60 s sem cliente (`LUCERNA_IDLE_TIMEOUT_MS` muda).

## Linha de comando

- `lucernad status`: o estado de todos os tópicos.
- `lucernad watch system.stats`: cada mudança de um tópico, até o Ctrl+C.
- `lucernad player org.mpris.MediaPlayer2.spotify`: escolhe o player cujo som o espectro mostra (sem nome, nenhum); `lucernad watch spectrum.bands` mostra as bandas.
- `lucernad historico [busca]`: os 20 itens mais recentes da área de transferência (com busca, só os que casam).
- `lucernad copiar ID`: põe um item do histórico na área de transferência.
- `lucernad brilho [ID PERCENT|+N|-N]`: as telas com brilho ajustável (com ID e PERCENT, ajusta uma; `+N` e `-N` somam ao valor atual).
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
cargo clippy -p lucernad --no-default-features --features power --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Para rodar o daemon sem o systemd: `cargo run -p lucernad -- serve`.

A sonda do D-Bus do `power` (`crates/lucernad-power/tests/sonda.rs`) também só roda num container, num D-Bus do sistema de mentira, nunca no logind, no UPower nem nos perfis da máquina:

```sh
docker run --rm -v "$PWD:/src:ro" -v <toolchain>:/tc:ro archlinux:latest \
  bash -c 'pacman -Syu --noconfirm --needed dbus gcc > /dev/null \
    && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-power/tests/isolado.sh'
```

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
