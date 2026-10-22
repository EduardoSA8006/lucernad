# lucernad: o módulo de energia e brilho — plano de implementação

> **Para agentes:** SUB-SKILL OBRIGATÓRIO: use superpowers:subagent-driven-development (recomendado) ou superpowers:executing-plans para implementar este plano tarefa por tarefa. Os passos usam checkbox (`- [ ]`) para acompanhamento.

**Objetivo:** o módulo `power` do `lucernad` (brilho da tela integrada pelo sysfs e pelo logind, brilho dos monitores externos por DDC/CI em código próprio, plugar e tirar pelo uevent do kernel, histórico de consumo e perfis pelo D-Bus) e o shell lendo dele, com o `brightnessctl`, o `ddcutil` e o `busctl` fora.

**Arquitetura:** dois repositórios. `lucernad`: o crate novo `lucernad-power` (a leitura do sysfs e a descoberta dos conectores como funções sobre uma raiz, o protocolo DDC/CI puro sobre o trait `I2cBus`, o juntar de ajustes como máquina de estado pura, o uevent do kernel por netlink numa thread, o D-Bus atrás do trait `PowerBus`, e o laço guiado pelo assinante) e a feature `power` (padrão) no binário, com o subcomando `brilho`, a regra udev e o `modules-load`. `lucerna-shell`: o `Brightness.qml` e a parte de histórico/perfis do `Battery.qml` viram adaptadores; a fumaça usa uma tela integrada falsa (`LUCERNAD_POWER_ROOT`). O `lucerna-sdk` não muda.

**Tecnologia:** Rust 1.95 (edição 2024); `i2cdev` 0.6 (0.6.2, traz o `nix` 0.26; o `ioctl` fica dentro dele: o crate do módulo compila com `unsafe_code = "deny"`, conferido na sonda); `rustix` 1 (já no workspace; ganha a feature `net` para o socket `NETLINK_KOBJECT_UEVENT`); `zbus` 5 (já no workspace, `tokio`, sem features novas); `tokio`, `serde_json`, `lucernad-core`, `lucerna-service` do SDK; QML do Quickshell 0.3.2; Hyprland 0.56.2 (container do `dev/test.sh`), sway 1.12 headless (fumaça), KWin 6.8 (a sessão do usuário); `dbus-daemon` só no container da sonda; GitHub Actions; `gh`.

**Spec:** `docs/superpowers/specs/2026-10-22-energia-e-brilho-design.md` (neste repositório, PR #13; leia inteiro antes da primeira tarefa). Base: os planos do espectro e da área de transferência (`docs/superpowers/plans/2026-10-21-area-de-transferencia.md`), cujos padrões este plano segue.

## Global Constraints

- **Ordem de execução:** Tarefas 1, 2, **2a** (a sonda real do i2c, feita pelo agente: nada se constrói sobre o `I2cBus` antes dela), 3–6 (`lucernad`, branch `feat/power`) → Tarefas 8–9 (`lucerna-shell`, branch `feat/energia-e-brilho`; só dependem do Formato e podem andar em paralelo com 1–6) → Tarefa 7 (conferência na máquina real, precisa do binário da 6; PR e merge do `lucernad`) → Tarefa 10 (precisa do `<LUCERNAD_REV>`; PR e merge do shell). Os números seguem o repositório, não a ordem.
- **Branches:** no `lucernad`, `git fetch` e confirme que o `origin/main` já tem o spec (o PR #13, com auto-merge); a `feat/power` parte dele, e o primeiro commit dela é este plano (feito pelo controlador). No `lucerna-shell`, `git switch -c feat/energia-e-brilho origin/main` no checkout principal (o `dev/test.sh` monta esse checkout).
- **Merge:** cada PR é aberto pela tarefa que fecha o repositório; o merge é do **controlador**, por merge commit (`gh pr merge <n> --merge`), depois da revisão final da branch (Opus) e do CI verde. Os rulesets exigem `fmt`/`clippy`/`test` (`lucernad`) e `lint`/`rules`/`unit`/`smoke` (shell).
- Contas e remotes: `gh` com a conta `EduardoSA8006`; o `origin` é `github-pessoal:lucerna-desktop/<repo>.git`.
- Toolchain: `cargo`/`rustc` em `~/develop/cargo/bin` (ponha no `PATH`); `rust-toolchain.toml` com `channel = "1.95"`. O SDK continua em `rev = "f55ba66dc18b62fab075b20691ad9edb7ca22a1c"`. No zsh, `git show "${R}:crates/..."`.
- Dependências novas só em `[workspace.dependencies]`, exatamente: `i2cdev = "0.6"`, `lucernad-power = { path = "crates/lucernad-power" }`; o `rustix` existente passa a `features = ["fs", "event", "net"]`. `unsafe_code = "deny"` vale para tudo (o `i2cdev`, o netlink do `rustix`, o `inotify` e o `zbus` compilaram sem `unsafe` no crate, na sonda de 2026-10-22). **Sem dependência de sistema nova:** nada de `libudev`; o CI não ganha `apt-get`.
- **Nenhum processo externo** em `crates/*/src`: nada de `std::process::Command` nem `tokio::process` (nada de `brightnessctl`, `ddcutil`, `busctl`, `udevadm`).
- **Nunca tocar no brilho, nos monitores, no logind nem nas permissões do usuário fora da Tarefa 7:**
  - os testes Rust usam só `testing::fake_config` (raiz falsa num `TempDir`, `FakeI2c`, `FakeBus`, `FakeUevents`); `PowerConfig::system()`, `LinuxI2c` e `ZbusPower::system()` só aparecem no `Modules::real()` e no `tests/sonda.rs`. Verificação de toda tarefa Rust: `grep -rnE "PowerConfig::system|LinuxI2c|ZbusPower" crates/*/tests crates/*/src/testing.rs` → só `crates/lucernad-power/tests/sonda.rs`;
  - os testes de processo (`crates/lucernad/tests/process.rs`) rodam o binário com `LUCERNAD_POWER_ROOT` = uma raiz falsa e `DBUS_SYSTEM_BUS_ADDRESS=unix:path=<tmp>/sem-barramento` (com o `env_clear` de hoje, o binário iria ao `/sys`, ao `/dev/i2c-*` e ao D-Bus do sistema de verdade);
  - a sonda do D-Bus (`tests/sonda.rs`) roda só pelo `tests/isolado.sh`, num container, com um `dbus-daemon` próprio; a trava `testing::isolated()` (a do clipboard) recusa fora dele;
  - **nenhum teste automatizado abre `/dev/i2c-*`**: o DDC de verdade só roda na Tarefa 7, conduzido pelo agente com o usuário (Global Constraints e passos de lá);
  - **no container do `dev/test.sh`, o `lucernad` sobe sempre com `LUCERNAD_POWER_ROOT`**: o `dev/test.sh` monta o D-Bus do sistema do host (`/run/dbus/system_bus_socket`) e o `/sys` do host é visível; sem a raiz falsa, um `power.set` no container chamaria o `SetBrightness` do logind de verdade (a sonda (b) mostrou que o `user/self` de um processo do uid 1000 cai na sessão gráfica do usuário). Defesa no próprio daemon: em container (`/.dockerenv` ou `/run/.containerenv`) **sem** `LUCERNAD_POWER_ROOT`, o `PowerConfig::system()` deixa o módulo indisponível (Tarefa 5). Antes de qualquer ajuste de brilho no container, `dev/test.sh run 'tr "\0" "\n" < /proc/$(pgrep -xo lucernad)/environ | grep ^LUCERNAD_POWER_ROOT='` mostra a raiz falsa; se não mostrar, pare.
- Tópicos, métodos, textos de erro e limites exatamente como em "Formato".
- Textos em português do Brasil, com acentos (mensagens, comentários, documentação, commits no estilo `Energia: o protocolo DDC/CI sobre o I2cBus`); nomes de código em inglês.
- Regras do shell: `qmllint` com 0 avisos; 100% das funções do escopo de `tests/coverage.json` chamadas pelos testes; camadas do `ci/rules.py`; funções declaradas como `function nome(...) {` numa linha; um teste que muda um singleton restaura o estado no fim; **todo dado que chega do daemon passa por `Array.from` antes de `map`/`filter`/`find`** (o bug do PR #8); **disponível = `client.connected && error === ""`** (a lição do PR #49: o `available` do status não diz se o módulo funciona).
- Verificação de toda tarefa Rust, além do teste dela: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo clippy -p lucernad --no-default-features --all-targets --locked -- -D warnings`, o mesmo com `--features system`, `spectrum`, `clipboard` e (da Tarefa 6 em diante) `power`, e `TMPDIR=/tmp cargo test --workspace --locked` limpos. O `Cargo.lock` atualizado vai no commit da tarefa que põe a dependência. Toda tarefa do shell: `dev/ci.sh lint rules unit` (e `smoke` na Tarefa 10) verdes.

## Review Focus

1. **Segurar a tecla num monitor DDC lento** (20 `power.change` de +5 em 200 ms, cada escrita levando 300 ms): no máximo uma escrita em voo, o valor final = o de partida + a soma, limitado a 100, e nenhum valor intermediário publicado (o slider não pula para trás). Testes: `change_accumulates_over_the_pending_target` (Tarefa 3) e `burst_of_changes_coalesces_on_a_slow_monitor` (Tarefa 5).
2. **Monitor que não responde, responde lixo ou trava a thread** (checksum errado, "null message", resposta curta, `read` que nunca volta): ele fica na lista com o erro dele, a tela integrada e o outro monitor seguem ajustando. Testes: `null_and_bad_replies_are_retried_then_fail` (Tarefa 2) e `dead_monitor_keeps_its_row_and_the_others_work` (Tarefa 5).
3. **Barramento que não é de placa de vídeo** (o SMBus `i801` de classe `0x0c0500`, o DesignWare do touchpad, sensores), mesmo com permissão dada por outra regra (o `45-i2c-tools.rules` do usuário dá o grupo `i2c` a todos): nunca é aberto; e nos de vídeo só se fala com 0x37 e 0x50, e só se grava o VCP 0x10. Testes: `only_gpu_buses_are_candidates` (Tarefa 1), `only_brightness_is_ever_written` (Tarefa 2), `opener_never_sees_a_non_gpu_bus` (Tarefa 5), `udev_rule_only_matches_gpu_classes` (Tarefa 6) e o passo das ACL da Tarefa 7.
4. **Tela integrada com o painel desligado** (a tampa fechada da máquina do usuário: `eDP-1` `disabled`, `bl_power` 4, `actual_brightness` 0 com `brightness` 19200) **e mudança de fora** (o slider do Plasma, a tecla do firmware): o valor vem do `brightness`, e a mudança chega ao tópico pelo uevent `change` do backlight. Testes: `value_comes_from_brightness_not_actual` (Tarefa 1) e `external_change_reaches_the_topic` (Tarefa 5).
5. **Dois monitores com o mesmo EDID na mesma placa, ou plugar e tirar em rajada:** nenhum barramento é chutado (os dois ficam com o erro do EDID igual) e a rajada de uevents vira uma descoberta só, sem nenhuma leitura de i2c quando nenhum conector mudou (os barramentos já casados nem entram como candidatos). Testes: `identical_edids_are_not_guessed` (Tarefa 1) e `hotplug_burst_rediscovers_once` (Tarefa 5).

---

## Sondas feitas antes do plano (2026-10-22, só leitura)

| Sonda | O que se viu |
| --- | --- |
| (a) crates | Num projeto descartável com o `rust-toolchain.toml` e `unsafe_code = "deny"`: `i2cdev` 0.6.2 (`LinuxI2CDevice::new`, `I2CDevice::{write, read}`) compila e roda; o `rustix` 1.1.5 com `net` abre e faz `bind` de um `NETLINK_KOBJECT_UEVENT` no grupo 1 **sem root**; o `inotify` do `rustix` vigia o `actual_brightness`; o `zbus` 5.19 serve e chama um `SetBrightness(ssu)` falso. O crate `udev` exige `libudev` (dependência de sistema): descartado. |
| (b) logind | `Session.SetBrightness(ssu)` existe; de um serviço de usuário (`systemd-run --user`), `session/auto` e `user/self`.`Display` dão a sessão gráfica (`"2"`, `/org/freedesktop/login1/session/_32`, `Type` `wayland`, `Active`); a sessão `manager` (a do systemd do usuário) não tem assento. |
| (c) DRM e i2c | Tampa fechada: `card2-eDP-1` `disabled`, `intel_backlight` `type` `raw`, `max` 19200, `brightness` 19200, **`actual_brightness` 0**, `bl_power` 4, `device` → `card2-eDP-1`. `card1-HDMI-A-1` (NVIDIA, PCI `0000:01:00.0`) `connected`, **sem** `ddc`, `edid` de 256 bytes (nome "LG ULTRAWIDE" no descritor 0xFC; o `stat` dá tamanho 0, só lendo se sabe). Intel: `card2-DP-1`/`DP-2`/`eDP-1` com `ddc` → `i2c-14`/`15`/`13` (AUX, filhos do conector, sem `class` no pai direto). Barramentos com ancestral de classe `0x030000`: `i2c-0`, `i2c-1` (NVIDIA) e `i2c-4`…`i2c-15` (i915); fora: `i2c-2`/`i2c-3` (DesignWare, `0000:00:15.*`) e `i2c-16` (SMBus I801, `0x0c0500`). O `i2c_dev` carrega por `/usr/lib/modules-load.d/{ddcutil,i2c-tools,fwupd-i2c}.conf`. |
| (d) UPower e perfis | `battery_BAT1` (`Type` 2, `PowerSupply` true); `GetHistory("rate", 3600, 120)` → `a(udu)`, 29 pontos, **o mais novo primeiro**, estado 1/4 vistos, taxa 0 no estado 4. `net.hadess.PowerProfiles` e `org.freedesktop.UPower.PowerProfiles` (mesmo dono, `power-profiles-daemon` 0.30): `Profiles` `aa{sv}` com a chave `Profile` (`power-saver`, `balanced`, `performance`), com `PropertiesChanged`. |
| (e) regra do ddcutil | `60-ddcutil-i2c.rules` (ddcutil 3.0.2): `SUBSYSTEM=="i2c-dev", KERNEL=="i2c-[0-9]*", ATTRS{class}=="0x03*", TAG+="uaccess"`; as ACL do usuário estão exatamente nos barramentos de vídeo (`i2c-0`, `1`, `4`–`15`). O `45-i2c-tools.rules` dá `GROUP="i2c"` a todos (o usuário não está no grupo). |
| (f) fumaça e container | A fumaça roda sem D-Bus do sistema e sem backlight; o container do `dev/test.sh` monta o D-Bus do sistema do host e vê o `/sys` do host (não tem `/dev/i2c-*`). Daí a raiz falsa por `LUCERNAD_POWER_ROOT` (Decisões). |

## Decisões deste plano (o que o spec deixava ao construtor)

| Tema | Decisão | Por quê |
| --- | --- | --- |
| Crate | `crates/lucernad-power` (biblioteca, feature `power` padrão no binário), com `testing` como os outros | o padrão da casa |
| Raiz | `Roots { sysfs, dev }` (`/sys`, `/dev`); toda leitura de sysfs e todo caminho de `/dev/i2c-N` sai dela | os testes e a fumaça sobre árvores falsas |
| Valor da tela integrada | do `brightness` (o pedido), não do `actual_brightness` | com o painel desligado o `actual_brightness` dá 0 (sonda c). **Para o PR** |
| Dispositivo | `firmware`, depois `platform`, depois `raw`; no mesmo tipo, o primeiro em ordem de nome; `output` = o conector do link `device` (`card2-eDP-1` → `eDP-1`), se houver | como o `brightnessctl`; o `output` deixa o shell casar a integrada com o monitor em foco |
| Mudanças de fora e plugar | um socket `NETLINK_KOBJECT_UEVENT` (grupo 1, o do kernel) numa `std::thread` (`recv` bloqueante com `SO_RCVTIMEO` de 1 s, encerra quando o canal fecha), só mensagens com `pid` 0 na origem, buffer de 8 KiB (mensagem truncada é descartada); `ENOBUFS` no `recv` (eventos perdidos) não é erro: vira um evento `Lost` que faz reler a integrada e redescobrir; `SUBSYSTEM=backlight` com o último componente do `DEVPATH` igual ao dispositivo escolhido → relê; `SUBSYSTEM=drm` → nova descoberta depois de `HOTPLUG_DELAY`; só enquanto houver assinante do `power.screens` | um mecanismo para os dois casos, sem `libudev` (sonda a); o kernel manda `change` com `SOURCE=sysfs` a cada escrita no backlight. Sem `inotify` |
| Escrita na integrada | `SetBrightness("backlight", <dispositivo>, <valor>)` no objeto da propriedade `Display` de `/org/freedesktop/login1/user/self` (sem sessão gráfica, o caminho `/` → erro); o caminho guardado e relido depois de um erro | sonda (b) |
| Raiz de teste | `LUCERNAD_POWER_ROOT=<pasta>`: `Roots` sob ela, a integrada escrita direto no arquivo `brightness` da árvore falsa (o `TreeWriter`, nunca o logind) e sem DDC; variável vazia = ausente; caminho relativo, inexistente, `/` ou dentro de `/sys`, `/dev` ou `/proc` (depois do `canonicalize`, comparando por componente com `Path::starts_with`) → o módulo fica indisponível com o erro do Formato, **nunca** volta à máquina de verdade; o `TreeWriter` faz `canonicalize` do arquivo de destino e recusa o que cair fora da raiz (um link para o `/sys` real); com a raiz definida, o daemon escreve na partida `lucernad: LUCERNAD_POWER_ROOT=<pasta>: o brilho é de teste (árvore falsa, sem logind nem DDC)`; **em container sem a raiz**, o módulo fica indisponível com o erro do Formato | a fumaça e o container sem tocar no brilho do host; sem processo externo. **Para o PR** |
| Ids | integrada `backlight`; monitor `ddc:<conector>` (`ddc:HDMI-A-1`) | o número do barramento muda entre partidas; o conector não |
| Conectores | só `status` = `connected`; fora `eDP*`, `LVDS*`, `DSI*` e `Writeback*`; o nome tira o `cardN-` | o eDP é a integrada |
| Barramento | com o link `ddc`, ele; sem, os candidatos são os `i2c-dev` cujo dispositivo fica sob o mesmo dispositivo PCI da placa **e** tem um ancestral com `class` começando por `0x03`, menos os que já são `ddc` de algum conector **e os já casados no cache**; lê-se o EDID (0x50, offset 0, 128 bytes) de cada candidato e casa com os 128 primeiros bytes do `edid` do conector; um só casamento vale; nenhum ou mais de um → erro | sonda (c); nunca chutar (Review Focus 5) |
| Cache | o barramento achado fica guardado por conector + hash do EDID; uma nova descoberta só lê i2c dos conectores novos ou com EDID diferente, e só nos barramentos fora do cache; sem conector novo, zero leituras de i2c (a releitura do VCP é preguiçosa: linha abaixo) | plugar não reconsulta os outros monitores |
| Nome | o texto do descritor 0xFC do EDID do sysfs (até 13 bytes, até o `0x0A`), só ASCII imprimível (0x20–0x7E), sem espaços nas pontas; vazio → "Monitor <conector>"; a integrada é "Tela integrada" | o número de série do EDID nunca sai do daemon |
| DDC/CI | escrita em 0x37: `[0x51, 0x80 \| n, payload…, chk]`, `chk` = XOR de `0x6E` e de tudo antes; Get VCP `[0x01, 0x10]`, espera `GET_DELAY` (40 ms), lê 11 bytes `[0x6E, 0x88, 0x02, resultado, 0x10, tipo, maxH, maxL, atualH, atualL, chk]`, `chk` = XOR de `0x50` e dos 10 primeiros; "null message" `[0x6E, 0x80, 0xBE]` = ocupado; Set VCP `[0x03, 0x10, valH, valL]`, espera `SET_DELAY` (50 ms) e relê pelo Get; 3 novas tentativas (`RETRIES`) com `RETRY_DELAY` (100 ms) quando não responde ou responde fora do padrão; resultado 1 = não suportado, sem nova tentativa; o comprimento vem do segundo byte (`& 0x7F`), então a null message cabe num buffer de 11 bytes; cada transação (escrever e ler) segura o `flock(LOCK_EX)` do `/dev/i2c-N` pelo `rustix::fs::flock`, o mesmo que o libddcutil pega | o padrão DDC/CI 1.1 e os atrasos do `ddcutil`; o `flock` evita atropelar o PowerDevil do KDE e o `ddcutil` no mesmo barramento |
| Uma thread por monitor | cada monitor DDC tem uma `std::thread` dona do `I2cBus` dele, com os comandos por `std::sync::mpsc` e a resposta por `oneshot`; o laço espera cada resposta até `op_timeout` (`OP_TIMEOUT`, 3 s); passado o prazo, o monitor fica com o erro "não respondeu" e o próximo pedido espera a thread voltar; uma thread presa num `read` do driver nunca é morta (vazamento aceito e anotado: some quando o monitor sai ou o daemon termina) | nada trava o laço nem os outros monitores (Review Focus 2) |
| Juntar | por tela, o `Adjuster` (Tarefa 3): `set`/`change` recebem **percentual**, `Write`/`Publish` levam o valor **bruto**; no máximo uma escrita em voo; um pedido no meio só troca o alvo; o `change` soma ao alvo pendente (ou ao em voo, ou ao atual); o mínimo é 1% (`raw = max(1, round(p × max / 100))`); depois da última escrita, a releitura é publicada; as releituras do meio, não. Antes de escrever num monitor DDC com erro, com `max` nulo ou com a última leitura há mais de `STALE` (2 s), um `get_brightness` primeiro; um sucesso apaga o erro (o monitor em standby na descoberta não fica morto) e dá a base certa do `change` | segurar a tecla soma tudo e o slider não pula para trás (Review Focus 1) |
| Trabalho | cada tela com escrita pendente segura um `WorkGuard` (o `register` recebe o `WorkTracker`, como o clipboard) | o `lucernad brilho ID N` sai antes de o DDC terminar; o daemon não pode sair por ociosidade no meio |
| Sem assinante | os métodos funcionam: o primeiro `power.set`/`change` sem descoberta feita a faz antes; o `power.list` redescobre sempre (barato com o cache); o uevent, o histórico e os perfis só com assinante dos tópicos deles | o "só com assinante" da casa; a linha de comando funciona sozinha |
| Histórico | a primeira bateria de `EnumerateDevices` com `Type` 2 e `PowerSupply` verdadeiro; `GetHistory("rate", 3600, 120)` ao ganhar assinante e a cada 60 s; pontos do mais antigo ao mais novo, sem taxa negativa nem não finita, no máximo 500; sem bateria → `{points: []}`; erro do D-Bus → `{points: [], error}`, com o texto do erro cortado em 300 caracteres (o mesmo corte em todo erro vindo do D-Bus antes de publicar) | o gráfico de hoje; `error` deixa o shell dizer por quê. **Para o PR** (o `error`) |
| Perfis | prefere `org.freedesktop.UPower.PowerProfiles` (`/org/freedesktop/UPower/PowerProfiles`), senão `net.hadess.PowerProfiles` (`/net/hadess/PowerProfiles`); acompanha o `NameOwnerChanged` dos dois e o `PropertiesChanged` do `Profiles`; sem D-Bus do sistema, nova tentativa a cada 30 s enquanto houver assinante | o serviço que sobe depois (a pendência do shell) |
| Prazos | D-Bus 2 s por chamada; `HOTPLUG_DELAY` 2,5 s (o de hoje no shell) | nada trava o laço |
| Método além do spec | `power.list {}` → `{screens}` (o mesmo do tópico, com a descoberta feita se preciso) | a linha de comando sem assinar. **Para o PR** |
| Linha de comando | `lucernad brilho` (`power.list`, uma linha `id  N%  nome` por tela, e `  (erro)` quando houver; "Nenhuma tela com brilho ajustável." sem telas) e `lucernad brilho ID PERCENT` (`power.set`; `+N`/`-N` → `power.change`); o texto passa pelo `plain()` (o que hoje está no clipboard sobe para fora do `cfg`) | a conferência e a fumaça sem o shell; nome de EDID e erro nunca mexem no terminal |
| Regra udev | `crates/lucernad/data/60-lucernad-i2c.rules` com a linha do ddcutil (`ATTRS{class}=="0x03*"`) e o comentário do porquê; `crates/lucernad/data/lucernad-i2c.conf` com `i2c-dev` para `/usr/lib/modules-load.d/`; o README instala os dois com `sudo` | sonda (e): a mesma forma, que só pega os barramentos de vídeo (o `uaccess` vale até a regra 73) |
| Shell: perfis sem o lucernad | o `hasPerformance` continua o `PowerProfiles.hasPerformanceProfile` do Quickshell; o `profilesAvailable` vem do `power.profiles` com conexão e, sem ela, fica com o último valor conhecido (não zera) | decisão do controlador: a troca de perfil é do Quickshell e não pode sumir com o daemon fora |
| Shell: donos | `Brightness` assina como dono `"power"` (`power.status`, `power.screens`); `Battery` como `"battery"` (`power.status`, `power.profiles` e, com `historyActive`, `power.history`) | o `want` por dono do cliente |
| Shell: valor otimista | o ajuste local muda o valor na hora; por `holdMs` (1000 ms) depois do último ajuste local de uma tela, o valor que chega do daemon para ela é ignorado | uma publicação de outra tela no meio do arrasto não volta o slider |
| Shell: telas com erro | ficam em `screens` com `error`; saem do `focusedScreen` (as teclas e o slider da central vão para a próxima); nas configurações, a linha mostra o erro e o slider fica desabilitado | "fica com o estado e o erro dele" |
| Shell: indisponível | sem `lucernad`, o slider da central some (sem tela, como hoje sem `brightnessctl`); a seção "Brilho" das configurações mostra o texto "Indisponível…" do core; o gráfico mostra o mesmo texto no lugar do "Medindo" | o padrão "Indisponível" das outras features |
| Fumaça | `ci/power-root.sh <pasta>` cria a árvore falsa (`sys/class/backlight/lucerna_teste`, `type` `raw`, `max_brightness` 1000, `brightness` e `actual_brightness` 500); o mesmo script serve ao `dev/hyprland.lua` | um só formato de raiz falsa |

## Formato

```jsonc
// power.screens — tópico comum; null até a primeira descoberta
{"screens":[
  {"id":"backlight","kind":"backlight","output":"eDP-1","name":"Tela integrada","value":9600,"max":19200,"percent":50},
  {"id":"ddc:HDMI-A-1","kind":"ddc","output":"HDMI-A-1","name":"LG ULTRAWIDE","value":70,"max":100,"percent":70},
  {"id":"ddc:DP-1","kind":"ddc","output":"DP-1","name":"Monitor DP-1","value":null,"max":null,"percent":null,
   "error":"Sem acesso ao barramento do monitor: instale a regra udev do lucernad (veja o README)."}
]}
// power.history — tópico comum; null até a primeira leitura
{"points":[{"time":1792682797,"rate":4.312,"state":1}]}          // com "error": "…" quando o D-Bus falha
// power.profiles — tópico comum; null até a primeira leitura
{"available":true,"performance":true}
// power.status — tópico comum
{"available":true,"error":null}
```

- `output` só existe quando se sabe o conector; `error` só existe com erro; `value`/`max`/`percent` são `null` quando nunca se leu.
- Métodos: `power.set {id, percent}` → `{}`; `power.change {id, delta}` → `{}`; `power.list {}` → `{screens}`.
- Erros `-32602` (`RpcError::invalid_params`, texto **sem** ponto final): `"id precisa ser o texto de uma tela, com até 64 caracteres"`, `"percent precisa ser um inteiro entre 1 e 100"`, `"delta precisa ser um inteiro entre -100 e 100"`.
- Erros do app: `1001` "Essa tela não existe." (o id não é ecoado: vai para o terminal); `1002` "O módulo de energia está indisponível: <error do status>.".
- `error` de uma tela, os textos: "Sem acesso ao barramento do monitor: instale a regra udev do lucernad (veja o README).", "Sem o /dev/i2c-<n>: carregue o módulo i2c-dev (veja o README).", "Sem o módulo i2c-dev: os monitores externos precisam dele (veja o README).", "Não achei o barramento deste monitor.", "Não deu para saber qual barramento é o deste monitor (outro monitor tem o mesmo EDID).", "O monitor não respondeu ao DDC/CI.", "O monitor respondeu fora do padrão DDC/CI.", "O monitor não deixa ajustar o brilho pelo DDC/CI.", "Não consegui ler o brilho da tela integrada: <e>.", "Não consegui ajustar o brilho pelo logind: <e>.", "O logind não tem uma sessão gráfica deste usuário.".
- `power.status.error`: "O módulo de energia parou por um erro interno.", "LUCERNAD_POWER_ROOT inválido: precisa ser uma pasta de teste fora do /sys, do /dev e do /proc.", "Em container, o módulo de energia só roda com LUCERNAD_POWER_ROOT (uma árvore falsa): sem ela, o brilho do host mudaria.".
- `power.history.error`: "Não consegui ler o histórico do UPower: <e>.".

## Mapa de arquivos

**`lucernad`:**

```
Cargo.toml  Cargo.lock  README.md  .github/workflows/ci.yml
crates/lucernad-power/
  Cargo.toml
  src/lib.rs        PowerConfig, PowerTopics, register, Power, o laço, os métodos
  src/roots.rs      Roots, root_from_env, read_small
  src/backlight.rs  Backlight, choose, read_value
  src/drm.rs        Connector, connected, candidate_buses, edid_name, BusMatch, match_edid
  src/i2c.rs        I2cBus, I2cOpener, LinuxI2c
  src/ddc.rs        Vcp, DdcError, DdcTiming, get_request, set_request, parse_reply, get_brightness, set_brightness, read_edid
  src/adjust.rs     Adjuster, Step, percent_of, raw_of
  src/uevent.rs     Uevent, parse_uevent, Uevents, KernelUevents
  src/bus.rs        PowerBus, BacklightWriter, Point, Profiles, points_from, profiles_from, ZbusPower, LogindWriter, TreeWriter
  src/testing.rs    (feature "testing") Tree, FakeI2c, FakeMonitor, FakeBus, FakeUevents, fake_config, quiet_config, isolated
  tests/{backlight,drm,ddc,adjust,uevent,bus,module,sonda}.rs  tests/isolado.sh
crates/lucernad/  Cargo.toml  data/{60-lucernad-i2c.rules,lucernad-i2c.conf}  src/{lib,main}.rs  tests/{common/mod.rs,protocol.rs,process.rs,udev.rs}
```

**`lucerna-shell`:** `services/Brightness.qml` (reescrito), `services/Battery.qml`, `features/central/state/PowerState.qml`, `features/central/ui/BatteryPage.qml`, `features/settings/state/IdleSettings.qml`, `features/settings/ui/IdlePage.qml`, `features/settings/ui/AboutPage.qml`, `tests/brightness.test.js` (reescrito), `tests/battery.test.js` (reescrito), `tests/coverage.json`, `ci/{power-root.sh,smoke.sh,tolerated.txt}`, `ci/tests/test_logcheck.py`, `dev/{lucernad.rev,Dockerfile,hyprland.lua,test.sh}`, `README.md`, `Lucerna — Pendências.md`, `Lucerna — Plugins Próprios.md`, `Lucerna — Proposta.md`.

---

### Tarefa 1: `lucernad` — a raiz, a tela integrada e os conectores

Repositório `lucernad`, branch `feat/power` (Global Constraints).

**Files:**
- Modify: `Cargo.toml` (`lucernad-power`, `rustix` com `net`)
- Create: `crates/lucernad-power/Cargo.toml` (`description = "Módulo power do lucernad: o brilho das telas, o histórico de consumo e os perfis de energia."`, `[features] testing = []`, ele mesmo com `testing` em `[dev-dependencies]`, `tempfile`), `src/lib.rs` (só os `pub mod` desta tarefa e a doc do crate), `src/{roots,backlight,drm}.rs`, `src/testing.rs` (o `Tree`)
- Test: `crates/lucernad-power/tests/{backlight,drm}.rs`

**Interfaces:**
- Consumes: nada.
- Produces:
  - `roots`: `#[derive(Clone, Debug)] pub struct Roots { pub sysfs: PathBuf, pub dev: PathBuf }` com `system()` e `at(base: &Path)` (`base/sys`, `base/dev`); `pub fn root_from_env(value: Option<&OsStr>, in_container: bool) -> Result<Option<Roots>, String>` (`None` ou vazio → `Ok(None)`, salvo `in_container` → `Err` com o texto do container; inválido pelas Decisões → `Err` com o texto do Formato); `pub fn in_container() -> bool` (`/.dockerenv` ou `/run/.containerenv`); `pub fn read_small(path: &Path) -> io::Result<String>` (até 4 KiB, sem espaços nas pontas); `pub const EDID_MAX: u64 = 32 * 1024`.
  - `backlight`: `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Backlight { pub device: String, pub max: u32, pub output: Option<String> }`; `pub fn choose(roots: &Roots) -> Option<Backlight>` (Decisões; `max` 0 ou ilegível → o dispositivo não vale); `pub fn read_value(roots: &Roots, device: &str) -> io::Result<u32>` (o `brightness`).
  - `drm`: `#[derive(Clone, Debug, PartialEq, Eq)] pub struct Connector { pub name: String, pub card: String, pub pci: PathBuf, pub ddc: Option<u32>, pub edid: Vec<u8> }`; `pub fn connected(roots: &Roots) -> Vec<Connector>` (Decisões; ordenados por nome; `edid` lido até `EDID_MAX`); `pub fn candidate_buses(roots: &Roots, pci: &Path, taken: &[u32]) -> Vec<u32>` (Decisões; ordenados; `taken` = os `ddc` de conectores mais os já casados no cache); `pub fn is_gpu_bus(roots: &Roots, bus: u32) -> bool` (o ancestral `0x03*`); `pub fn edid_name(edid: &[u8]) -> Option<String>`; `pub fn i2c_dev_loaded(roots: &Roots) -> bool` (`<sysfs>/class/i2c-dev` existe); `#[derive(Debug, PartialEq, Eq)] pub enum BusMatch { Found(u32), NotFound, Ambiguous }`; `pub fn match_edid(sysfs_edid: &[u8], read: &[(u32, Option<[u8; 128]>)]) -> BusMatch` (compara os 128 primeiros bytes; um `None` é um barramento que não respondeu).
  - `testing::Tree` (montador de árvore falsa sobre um diretório, com os links simbólicos de verdade): `Tree::new(dir: &Path) -> Tree`, `roots(&self) -> Roots`, `backlight(&self, device: &str, kind: &str, max: u32, value: u32, actual: u32, connector: Option<&str>)`, `gpu(&self, pci: &str, class: &str, card: &str)` (cria `sys/devices/<pci>` com o `class` e `sys/class/drm/<card>`), `connector(&self, card: &str, name: &str, status: &str, edid: &[u8], ddc: Option<u32>)`, `bus(&self, n: u32, parent: &str)` (o `i2c-dev` sob `sys/devices/<parent>`, com o `name`), `set_value(&self, device: &str, value: u32)`, `value(&self, device: &str) -> u32`; `testing::edid(name: &str, serial: u32) -> Vec<u8>` (128 bytes válidos com o descritor 0xFC).

- [ ] **Passo 1: os testes.**
  - `tests/backlight.rs`:
    - `firmware_beats_platform_beats_raw`: `raw` `intel_backlight`, `platform` `p`, `firmware` `acpi_video0` → `choose` = `acpi_video0`; sem o `firmware` → `p`; dois `raw` (`b`, `a`) → `a`.
    - `value_comes_from_brightness_not_actual`: `intel_backlight` `max` 19200, `brightness` 19200, `actual_brightness` 0 → `read_value` = 19200.
    - `output_from_the_device_link`: o `device` → `card2-eDP-1` → `output` `Some("eDP-1")`; sem `device` → `None`.
    - `zero_max_is_not_a_backlight`: `max_brightness` 0 → `choose` `None`; nenhuma pasta → `None`.
    - `env_root_is_checked`: `root_from_env(None, false)` e `(Some(""), false)` = `Ok(None)`; uma pasta temporária → `Ok(Some(_))` com `sysfs` = `<pasta>/sys`; `"rel"`, `"/"`, `"/sys/class"`, `"/dev"`, `"/proc/self"` e uma pasta inexistente → `Err` com "LUCERNAD_POWER_ROOT inválido"; uma pasta temporária chamada `sysfoo` sob o `TempDir` vale (comparação por componente).
    - `container_without_root_is_refused`: `root_from_env(None, true)` → `Err` com "Em container"; com uma pasta temporária e `true` → `Ok(Some(_))`.
  - `tests/drm.rs` (a árvore da sonda (c): NVIDIA `0000:01:00.0` com `card1`, `i2c-0` e `i2c-1`; Intel `0000:00:02.0` com `card2`, `card2-eDP-1` (`ddc` 13), `card2-DP-1` (`ddc` 14, `disconnected`) e `i2c-4`; `i2c-16` sob `0000:00:1f.4` de classe `0x0c0500`; `i2c-2` sob `0000:00:15.0/i2c_designware.0` sem `class` no pai e `0x0c8000` no avô):
    - `connected_skips_internal_and_disconnected`: `card1-HDMI-A-1` conectado → `connected` = só `HDMI-A-1`, `card` `card1`, `ddc` `None`, `edid` de 256 bytes; o `eDP-1` fica de fora.
    - `ddc_link_is_used`: `card2-DP-1` passando a `connected` → `ddc` `Some(14)`.
    - `only_gpu_buses_are_candidates`: `candidate_buses(roots, <pci da NVIDIA>, &[])` = `[0, 1]`; com `&[1]` (o LG já casado) → `[0]`; para a Intel com `taken` `[13, 14]` → `[4]`; `is_gpu_bus(16)`, `is_gpu_bus(2)` = `false`; `is_gpu_bus(13)` = `true` (o AUX sob o conector).
    - `edid_name_is_clean`: `edid_name(&edid("LG ULTRAWIDE", 1))` = `Some("LG ULTRAWIDE")`; com `\x1b[2J` no nome → só os imprimíveis; sem descritor 0xFC → `None`.
    - `edid_matching`: `match_edid(a, &[(0, None), (1, Some(a[..128]))])` = `Found(1)`; nenhum igual → `NotFound`.
    - `identical_edids_are_not_guessed`: os mesmos 128 bytes em dois barramentos → `Ambiguous`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-power` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-power --test backlight --test drm` → 12 passed; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Energia: a raiz, a tela integrada e os conectores pelo sysfs"`

---

### Tarefa 2: `lucernad` — o protocolo DDC/CI sobre o `I2cBus`

**Files:**
- Create: `crates/lucernad-power/src/{i2c,ddc}.rs`; em `src/testing.rs`, `FakeI2c` e `FakeMonitor`
- Modify: `Cargo.toml` (`i2cdev`), `crates/lucernad-power/Cargo.toml`
- Test: `crates/lucernad-power/tests/ddc.rs`

**Interfaces:**
- Consumes: `Roots` (Tarefa 1).
- Produces:
  - `i2c`: `pub trait I2cBus: Send { fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()>; fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()>; }`; `pub trait I2cOpener: Send + Sync + 'static { fn open(&self, bus: u32) -> io::Result<Box<dyn I2cBus>>; }`; `pub struct LinuxI2c { pub dev: PathBuf }` (abre `<dev>/i2c-<n>` com `LinuxI2CDevice::new` e troca o endereço com `set_slave_address`; se a Tarefa 2a mostrar que o `read`/`write` simples não serve no driver da NVIDIA, usa o `transfer` (`I2C_RDWR`) do `i2cdev`; nunca usado nos testes); `pub trait I2cBus` ganha `fn lock(&mut self) -> io::Result<()>` e `fn unlock(&mut self)` (no `LinuxI2c`, `rustix::fs::flock` `LockExclusive`/`Unlock` no fd do `/dev/i2c-N`; no fake, contadores).
  - `ddc`: `pub const DDC_ADDR: u16 = 0x37`, `EDID_ADDR: u16 = 0x50`, `VCP_BRIGHTNESS: u8 = 0x10`, `RETRIES: usize = 3`; `#[derive(Clone, Copy, Debug)] pub struct DdcTiming { pub get_delay: Duration, pub set_delay: Duration, pub retry_delay: Duration }` com `standard()` (40, 50, 100 ms); `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Vcp { pub current: u16, pub max: u16 }`; `#[derive(Debug, PartialEq, Eq)] pub enum DdcError { NoAccess, NoDevice(u32), NoAnswer, BadReply, Unsupported, Io(String) }` com `message(&self) -> String` (os textos do Formato; `NoAccess` de `EACCES`/`EPERM`, `NoDevice` de `ENOENT`).
  - `pub fn get_request(code: u8) -> Vec<u8>`; `pub fn set_request(code: u8, value: u16) -> Vec<u8>`; `pub fn parse_reply(code: u8, bytes: &[u8]) -> Result<Vcp, DdcError>` (o comprimento pelo segundo byte `& 0x7F`, os bytes além dele ignorados; null message, checksum, tamanho, opcode `0x02`, código diferente → `BadReply`; resultado 1 → `Unsupported`; `max` 0 → `BadReply`).
  - `pub fn get_brightness(bus: &mut dyn I2cBus, t: &DdcTiming) -> Result<Vcp, DdcError>` e `pub fn set_brightness(bus: &mut dyn I2cBus, value: u16, t: &DdcTiming) -> Result<Vcp, DdcError>` (grava, espera, relê; as novas tentativas pelas Decisões; erro de acesso e `Unsupported` sem nova tentativa; cada tentativa entre `lock` e `unlock`); `pub fn read_edid(bus: &mut dyn I2cBus) -> Result<[u8; 128], DdcError>` (escreve `[0x00]` em 0x50 e lê 128).
  - `testing::FakeMonitor { pub edid: Vec<u8>, pub current: u16, pub max: u16, pub off_by: i16 }` (`off_by`: o que o monitor guarda a mais ou a menos do que recebeu, para a releitura); `testing::FakeI2c` (`Clone`, um mapa barramento → estado; abrir um barramento sem monitor dá certo e toda leitura dele vem com zeros, como um barramento vazio de verdade): `new()`, `monitor(self, bus: u32, m: FakeMonitor) -> FakeI2c`, `denied(self, bus: u32)` (`open` → `EACCES`), `silent(self, bus: u32)` (lê só zeros), `busy_times(&self, bus: u32, n: usize)` (as próximas `n` leituras de VCP devolvem a null message), `corrupt_times(&self, bus: u32, n: usize)` (checksum errado), `slow(&self, bus: u32, d: Duration)` (cada operação demora `d`), `hang(&self, bus: u32)` (a próxima leitura nunca volta até `release(bus)`), `release(&self, bus: u32)`, `set_current(&self, bus: u32, v: u16)`, `current(&self, bus: u32) -> u16`, `opened(&self) -> Vec<u32>` (cada `open`), `writes(&self, bus: u32) -> Vec<(u16, Vec<u8>)>` (endereço e bytes de cada escrita), `locks(&self, bus: u32) -> (usize, usize)` (`lock`s e `unlock`s), `reads(&self, bus: u32) -> usize`; `impl I2cOpener for FakeI2c`; `testing::fast_timing() -> DdcTiming` (zeros).

- [ ] **Passo 1: os testes** (`tests/ddc.rs`; `lg()` = `FakeMonitor { edid: edid("LG ULTRAWIDE", 1), current: 70, max: 100, off_by: 0 }` no barramento 1).
  - `requests_match_the_standard`: `get_request(0x10)` = `[0x51, 0x82, 0x01, 0x10, 0xAC]`; `set_request(0x10, 70)` = `[0x51, 0x84, 0x03, 0x10, 0x00, 0x46, 0xEE]`.
  - `reply_parsing`: `parse_reply(0x10, &[0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x46, 0x86])` = `Ok(Vcp { current: 70, max: 100 })`; o mesmo com o último byte `0x87` → `BadReply`; `[0x6E, 0x80, 0xBE]` → `NoAnswer`, e o mesmo num buffer de 11 bytes completado com zeros (como sai do `read`) → `NoAnswer`; 5 bytes → `BadReply`; resultado `0x01` (checksum recalculado) → `Unsupported`.
  - `get_and_set_through_the_fake`: `get_brightness` = `Vcp { 70, 100 }`; `set_brightness(…, 40, …)` = `Vcp { 40, 100 }` e `current(1)` = 40.
  - `null_and_bad_replies_are_retried_then_fail`: `busy_times(1, 2)` → `get_brightness` `Ok`; `corrupt_times(1, 3)` → `Ok` (3 novas tentativas); `busy_times(1, 4)` → `Err(NoAnswer)`; `silent` → `Err(NoAnswer)` ou `Err(BadReply)` depois de 4 escritas do pedido em `writes`.
  - `access_errors_are_not_retried`: `denied(1)` → `open` com `PermissionDenied`, `DdcError` `NoAccess` e a mensagem do Formato.
  - `edid_is_read_at_0x50`: `read_edid` = os 128 primeiros bytes do `edid`, e a escrita foi `(0x50, [0x00])`.
  - `every_transaction_is_locked`: depois de `get`, `set` e `read_edid`, `locks(1)` com o mesmo número de `lock` e `unlock`, um por tentativa.
  - `only_brightness_is_ever_written`: depois de `get`, `set` e `read_edid`, toda escrita em `writes(1)` tem endereço 0x37 com `[0x51, _, 0x01, 0x10, _]` ou `[0x51, _, 0x03, 0x10, _, _, _]`, ou 0x50 com `[0x00]`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-power --test ddc` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-power --test ddc` → 8 passed; o workspace inteiro verde. Se um número de teste se mostrar errado pela conta (não pelo código), pare e relate antes de mudá-lo.
- [ ] **Passo 5: commit.** `git commit -m "Energia: o protocolo DDC/CI sobre o I2cBus, com nova tentativa"`

---

### Tarefa 2a: `lucernad` — a sonda real do i2c, só leitura (feita pelo agente)

Antes de construir em cima do `I2cBus`: confirmar que o `LinuxI2c` fala com o driver da NVIDIA pelo `read`/`write` simples. **Só leitura do brilho**: Get VCP 0x10 e o EDID, nunca Set VCP. Quem roda é o agente, na máquina do usuário, com o usuário avisado antes; nada automatizado.

**Files:**
- Create: `crates/lucernad-power/examples/sonda_i2c.rs` (`sonda_i2c <barramento>`: abre `LinuxI2c { dev: "/dev" }`, imprime o EDID lido (os 8 primeiros bytes em hex e o `edid_name`) e o resultado do `get_brightness`; recusa barramento que o `is_gpu_bus(&Roots::system(), n)` não aceite, com código 2; não tem nenhum caminho que chame `set_brightness`)
- Modify, se preciso: `crates/lucernad-power/src/i2c.rs` (o `transfer`/`I2C_RDWR` no lugar do `read`/`write`, com o teste da Tarefa 2 continuando verde)

**Interfaces:**
- Consumes: `LinuxI2c`, `get_brightness`, `read_edid`, `edid_name`, `is_gpu_bus`, `Roots` (Tarefas 1–2).
- Produces: a forma de transação que vale no hardware (anotada no relatório e no PR); nenhuma API nova.

- [ ] **Passo 1: o exemplo.** `grep -n "set_brightness\|set_request" crates/lucernad-power/examples/sonda_i2c.rs` → nada.
- [ ] **Passo 2: rodar no i2c-0 e no i2c-1** (o usuário avisado; o PowerDevil pode estar no barramento, e o `flock` o respeita): `cargo run --release -p lucernad-power --example sonda_i2c -- 0` e `-- 1`. Critério: num deles, o EDID com "LG ULTRAWIDE" e os 8 bytes `00 ff ff ff ff ff ff 00` iguais aos do `/sys/class/drm/card1-HDMI-A-1/edid`, e o Get VCP com `current`/`max` plausíveis; no outro, erro limpo (sem pânico). `cargo run … -- 16` → recusado com código 2, sem abrir o barramento.
- [ ] **Passo 3: se o `read`/`write` falhar** (ex.: `EIO`/`ENXIO` só na NVIDIA), trocar o `LinuxI2c` para o `transfer` com `LinuxI2CMessage` (`I2C_RDWR`) e rodar de novo; se nenhum servir, pare e relate.
- [ ] **Passo 4: commit.** `git commit -m "Energia: a sonda de leitura do i2c (EDID e Get VCP) no hardware"`

---

### Tarefa 3: `lucernad` — o juntar de ajustes e o uevent do kernel

**Files:**
- Create: `crates/lucernad-power/src/{adjust,uevent}.rs`; em `src/testing.rs`, `FakeUevents`
- Test: `crates/lucernad-power/tests/{adjust,uevent}.rs`

**Interfaces:**
- Consumes: nada.
- Produces:
  - `adjust` (unidades: `set`/`change` em **percentual**; `Step::Write`/`Step::Publish`, `new`, `written` e `external` em valor **bruto**): `pub fn raw_of(percent: u8, max: u32) -> u32` (Decisões); `pub fn percent_of(raw: u32, max: u32) -> u8` (arredondado, 0–100); `#[derive(Debug, PartialEq, Eq)] pub enum Step { Write(u32), Publish(u32), Failed(String), Idle }`; `pub struct Adjuster` com `new(current: u32, max: u32)`, `set(&mut self, percent: u8) -> Step` (`Write` se nada em voo, senão `Idle` com o alvo trocado), `change(&mut self, delta: i32) -> Step` (a base pela ordem alvo pendente, em voo, atual; limitado a 1–100), `written(&mut self, read: Result<u32, String>) -> Step` (com alvo pendente diferente do lido → `Write(alvo)`; sem alvo → `Publish(lido)` ou `Failed(e)`), `external(&mut self, read: u32) -> bool` (guarda e diz se publica: só sem nada em voo nem pendente), `busy(&self) -> bool`, `current(&self) -> u32`, `set_max(&mut self, max: u32)` (a releitura do DDC pode trazer outro máximo).
  - `uevent`: `#[derive(Debug, Clone, PartialEq, Eq)] pub enum Uevent { Event { action: String, subsystem: String, devpath: String }, Lost }` (`Lost` = `ENOBUFS` no `recv`: o kernel descartou eventos); `pub fn devpath_name(devpath: &str) -> &str` (o último componente); `pub fn parse_uevent(msg: &[u8]) -> Option<Uevent>` (o formato do kernel `ação@devpath\0CHAVE=valor\0…`; cabeçalho `libudev` → `None`); `pub trait Uevents: Send + Sync + 'static { fn watch(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<Uevent>, String>; }` (largar o receptor encerra a thread em até 1 s); `pub struct KernelUevents` (Decisões).
  - `testing::FakeUevents` (`Clone`): `new()`, `send(&self, action: &str, subsystem: &str, devpath: &str)`, `lose(&self)` (manda `Lost`), `watching(&self) -> usize` (receptores vivos).

- [ ] **Passo 1: os testes.**
  - `tests/adjust.rs`:
    - `percent_and_raw`: `raw_of(50, 19200)` = 9600; `raw_of(1, 100)` = 1; `raw_of(1, 50)` = 1 (nunca 0); `percent_of(9600, 19200)` = 50; `percent_of(19199, 19200)` = 100.
    - `set_while_writing_keeps_only_the_last`: `new(50, 100)`; `set(60)` = `Write(60)`; `set(70)` e `set(80)` = `Idle`; `written(Ok(60))` = `Write(80)`; `written(Ok(80))` = `Publish(80)`.
    - `change_accumulates_over_the_pending_target`: `new(50, 100)`; `change(5)` = `Write(55)`; mais 19 `change(5)` = `Idle`; `written(Ok(55))` = `Write(100)` (limitado); `written(Ok(100))` = `Publish(100)`; `change(-200)` → `Write(1)` (o mínimo).
    - `units_with_a_real_max`: `new(9600, 19200)`; `set(30)` = `Write(5760)`; `written(Ok(5760))` = `Publish(5760)`; `change(5)` = `Write(6720)` (35%); `change(-50)` → `Write(192)` (1%).
    - `failure_reports_and_frees`: `set(60)`, `written(Err("x"))` = `Failed("x")`, `busy()` falso; o próximo `set(60)` = `Write(60)`.
    - `external_change_publishes_only_when_idle`: `external(30)` = `true` e `current()` 30; com uma escrita em voo, `external(31)` = `false`.
  - `tests/uevent.rs`:
    - `parses_kernel_messages`: `b"change@/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight\0ACTION=change\0DEVPATH=/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight\0SUBSYSTEM=backlight\0SOURCE=sysfs\0SEQNUM=9\0"` → `Uevent::Event { "change", "backlight", "/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight" }` e `devpath_name` = `"intel_backlight"`; uma mensagem `drm` com `HOTPLUG=1` → `subsystem` `"drm"`.
    - `rejects_garbage`: `b"libudev\0…"`, `b""`, `b"sem arroba"`, sem `SUBSYSTEM` → `None`.
    - `kernel_socket_opens_without_root`: `KernelUevents.watch()` → `Ok`; largar o receptor e em 2 s a thread terminou (o contador de threads do módulo, `uevent::threads()`, volta ao que era). (Só abre o socket e escuta; não manda nada.)
    - `fake_delivers_and_counts`: `FakeUevents::send` chega ao receptor e `lose()` chega como `Lost`; `watching()` 1 e, largado, 0.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-power --test adjust --test uevent` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces (`uevent::threads() -> usize` é um contador `AtomicUsize` público só para o teste).
- [ ] **Passo 4: verificar.** `cargo test -p lucernad-power --test adjust --test uevent` → 10 passed; o workspace inteiro verde.
- [ ] **Passo 5: commit.** `git commit -m "Energia: o juntar de ajustes e o uevent do kernel por netlink"`

---

### Tarefa 4: `lucernad` — o D-Bus: logind, UPower e perfis

**Files:**
- Create: `crates/lucernad-power/src/bus.rs`; em `src/testing.rs`, `FakeBus` e `isolated`; `tests/isolado.sh`
- Modify: `crates/lucernad-power/Cargo.toml` (`zbus`, `tokio`)
- Test: `crates/lucernad-power/tests/{bus,sonda}.rs`

**Interfaces:**
- Consumes: `Roots` (Tarefa 1).
- Produces:
  - `pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>`; `pub const DBUS_TIMEOUT: Duration` (2 s), `PROFILES_RETRY` (30 s), `MAX_POINTS: usize = 500`.
  - `#[derive(Clone, Debug, PartialEq)] pub struct Point { pub time: u32, pub rate: f64, pub state: u32 }`; `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Profiles { pub available: bool, pub performance: bool }`.
  - `pub fn points_from(raw: Vec<(u32, f64, u32)>) -> Vec<Point>` (Decisões: ordena, filtra, limita); `pub fn profiles_from(list: &[HashMap<String, zbus::zvariant::OwnedValue>]) -> Profiles` (`available` true, `performance` = algum `Profile` = `"performance"`).
  - `pub trait BacklightWriter: Send + Sync + 'static { fn write<'a>(&'a self, device: &'a str, value: u32) -> BoxFuture<'a, Result<(), String>>; }` (o `Err` já é o texto do Formato).
  - `pub trait PowerBus: Send + Sync + 'static { fn history(&self) -> BoxFuture<'_, Result<Vec<Point>, String>>; fn watch_profiles(&self) -> tokio::sync::mpsc::UnboundedReceiver<Profiles>; }` (o `watch_profiles` manda o estado atual e cada mudança, e tenta de novo a cada `PROFILES_RETRY` sem D-Bus; largar o receptor encerra a tarefa).
  - `pub struct ZbusPower` com `system() -> ZbusPower` (conexão preguiçosa com o D-Bus do sistema, refeita depois de erro de E/S, como o `SessionBus` do espectro) e `impl PowerBus`; `pub struct LogindWriter` (`system()`, `impl BacklightWriter`; o caminho da sessão pelas Decisões); `pub struct TreeWriter { pub roots: Roots }` (`impl BacklightWriter`: grava `brightness` e `actual_brightness` da árvore; faz `canonicalize` de cada arquivo e recusa com `Err("o arquivo de brilho sai da raiz de teste")` o que não ficar sob a raiz canônica); `pub fn clip_error(e: &str) -> String` (no máximo 300 caracteres, cortando em fronteira de caractere, com "…"; todo erro vindo do D-Bus passa por ele).
  - `testing::FakeBus` (`Clone`): `new()`, `with_history(self, Vec<Point>) -> FakeBus`, `history_fails(&self, e: &str)`, `set_profiles(&self, p: Profiles)`, `history_calls(&self) -> usize`, `watching(&self) -> usize`, `panicking() -> FakeBus` (o `history` entra em pânico); `impl PowerBus`; `testing::FakeWriter` (`Clone`): `new(tree: Tree)` (grava na árvore, como o `TreeWriter`), `fails(&self, e: &str)`, `slow(&self, d: Duration)`, `writes(&self) -> Vec<(String, u32)>`; `impl BacklightWriter`. `testing::isolated()` (a trava do clipboard: `LUCERNA_ISOLADO=1` **e** `/.dockerenv` ou `/run/.containerenv`, senão pânico com "sonda.rs: só roda pelo tests/isolado.sh, num container").

**`tests/isolado.sh`** (`set -euo pipefail`, o cabeçalho com o uso e o `docker run` abaixo): recusa fora de container ("isolado.sh: só roda dentro de um container (sobe um D-Bus do sistema de mentira)"); cria um `XDG_RUNTIME_DIR` temporário (0700); sobe `dbus-daemon --config-file=<a config de sessão> --address=unix:path=$XDG_RUNTIME_DIR/system_bus --nofork` com `setsid` (a config de sessão deixa o teste ter os nomes `org.freedesktop.login1`, `org.freedesktop.UPower`, `net.hadess.PowerProfiles` e `org.freedesktop.UPower.PowerProfiles`); exporta `DBUS_SYSTEM_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/system_bus` e `LUCERNA_ISOLADO=1` só para o `cargo test`; copia o repositório (montado só para leitura) por `tar`, sem `target` nem `.git`; roda `cargo test -p lucernad-power --test sonda --locked -- --ignored --test-threads=1`; encerra o `dbus-daemon` num `trap`.

- [ ] **Passo 1: os testes.**
  - `tests/bus.rs` (rodam no CI):
    - `history_is_sorted_and_clean`: `points_from(vec![(3, 2.5, 4), (1, -1.0, 2), (2, f64::NAN, 1), (0, 4.3, 1)])` = `[Point{0,4.3,1}, Point{3,2.5,4}]`; 600 pontos → 500 (os mais novos).
    - `profiles_from_the_property`: a lista da sonda (d) (`power-saver`, `balanced`, `performance`) → `{true, true}`; sem o `performance` → `{true, false}`; vazia → `{true, false}`.
    - `tree_writer_writes_the_fake_file`: `Tree` com `lucerna_teste` (`max` 1000, valor 500) → `TreeWriter.write("lucerna_teste", 700)` → `tree.value` 700 e `actual_brightness` 700.
    - `tree_writer_refuses_links_out_of_the_root`: na árvore, `brightness` de `lucerna_teste` trocado por um link simbólico para um arquivo fora da raiz (num segundo `TempDir`) → `Err` com "sai da raiz" e o arquivo de fora intacto.
    - `long_dbus_errors_are_clipped`: `clip_error` de 5000 `"é"` → 300 caracteres terminando em "…"; um texto curto fica igual.
    - `sonda_refuses_outside_the_container`: sem `LUCERNA_ISOLADO`, `catch_unwind(isolated)` → `Err`.
  - `tests/sonda.rs` (todos `#[ignore = "só no D-Bus isolado (tests/isolado.sh)"]`, cada um começando por `isolated()`; a sonda registra no barramento isolado os serviços falsos por `#[zbus::interface]`: `/org/freedesktop/login1/user/self` com `Display` `("2", "/org/freedesktop/login1/session/_2")`, a sessão com `SetBrightness(ssu)` que guarda as chamadas, o UPower com `EnumerateDevices` (um `line_power_AC` e um `battery_BAT0`) e o `GetHistory` com os pontos da sonda (d), e o serviço de perfis):
    - `logind_receives_set_brightness`: `LogindWriter::system().write("intel_backlight", 9600)` → a sessão `_2` recebe `("backlight", "intel_backlight", 9600)`; com `Display` `("", "/")` → `Err` "O logind não tem uma sessão gráfica deste usuário.".
    - `history_from_the_battery`: `ZbusPower::system().history()` = os pontos em ordem crescente; sem `battery_*` → `Ok(vec![])`.
    - `profiles_follow_the_service`: `watch_profiles()` → primeiro `{true, true}`; o teste solta o nome `org.freedesktop.UPower.PowerProfiles` e o `net.hadess.PowerProfiles` → `{false, false}` em 2 s; registra de novo só o `net.hadess.PowerProfiles` sem o `performance` → `{true, false}`; muda o `Profiles` (com `PropertiesChanged`) → `{true, true}`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-power --test bus` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.**
  - `cargo test -p lucernad-power --test bus` → 6 passed; o workspace inteiro verde.
  - A sonda, no container isolado (sem nenhum socket do host):

    ```sh
    docker run --rm -v "$PWD:/src:ro" -v ~/develop/rustup/toolchains/1.95-x86_64-unknown-linux-gnu:/tc:ro archlinux:latest \
      bash -c 'pacman -Syu --noconfirm --needed dbus gcc > /dev/null \
        && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-power/tests/isolado.sh'
    ```

    Critério: 3 passed.
- [ ] **Passo 5: commit.** `git commit -m "Energia: o logind, o histórico do UPower e os perfis pelo D-Bus"`

---

### Tarefa 5: `lucernad` — o módulo `power`

**Files:**
- Modify: `crates/lucernad-power/src/lib.rs` (o módulo), `src/testing.rs` (`fake_config`, `quiet_config`), `crates/lucernad-power/Cargo.toml` (`lucernad-core`, `lucerna-service`, `serde_json`; nos testes, `lucerna-ipc`)
- Test: `crates/lucernad-power/tests/module.rs`

**Interfaces:**
- Consumes: Tarefas 1–4; `lucernad_core::{detached, Detached, supervise, publish_changed, ModuleTask}`; `lucerna_service::{ServerBuilder, Topic, RpcError, WorkTracker, WorkGuard}`.
- Produces:
  - `pub const OP_TIMEOUT: Duration` (3 s), `STALE` (2 s), `HOTPLUG_DELAY` (2,5 s), `HISTORY_INTERVAL` (60 s), `HISTORY_SPAN: u32 = 3600`, `HISTORY_RESOLUTION: u32 = 120`, `ID_MAX: usize = 64`.
  - `pub struct PowerConfig { pub roots: Roots, pub root_error: Option<String>, pub writer: Arc<dyn BacklightWriter>, pub i2c: Option<Arc<dyn I2cOpener>>, pub bus: Arc<dyn PowerBus>, pub uevents: Arc<dyn Uevents>, pub timing: DdcTiming, pub op_timeout: Duration, pub stale: Duration, pub hotplug_delay: Duration, pub history_interval: Duration }` com `pub fn system() -> PowerConfig` (pelo `root_from_env(env::var_os("LUCERNAD_POWER_ROOT").as_deref(), in_container())`, com o aviso da partida das Decisões quando há raiz: sem raiz → `Roots::system()`, `LogindWriter`, `LinuxI2c { dev: "/dev" }`; com raiz → `TreeWriter`, `i2c` `None`; inválida → `root_error` com o texto; sempre `ZbusPower::system()`, `KernelUevents`, `DdcTiming::standard()` e as constantes acima).
  - `#[derive(Clone)] pub struct PowerTopics { pub screens: Topic, pub history: Topic, pub profiles: Topic, pub status: Topic }`.
  - `pub fn register(server: &mut ServerBuilder, cfg: PowerConfig, work: WorkTracker) -> Power` (os quatro tópicos do Formato, nesta ordem, e os três métodos); `pub struct Power { pub topics: PowerTopics, /* privado */ }` com `pub fn spawn(self) -> ModuleTask` (o `supervise` com `"de energia"` e `{"available":false,"error":"O módulo de energia parou por um erro interno."}`).
  - `testing::fake_config(dir: &Path) -> (PowerConfig, Fakes)` com `pub struct Fakes { pub tree: Tree, pub i2c: FakeI2c, pub bus: FakeBus, pub uevents: FakeUevents, pub writer: FakeWriter }` (a árvore da Tarefa 1 com o `intel_backlight` em 9600/19200, o LG no `card1-HDMI-A-1` e no `FakeI2c` barramento 1; `fast_timing()`, `op_timeout` 300 ms, `hotplug_delay` 100 ms, `history_interval` 200 ms); `testing::quiet_config(dir: &Path) -> PowerConfig` (árvore vazia, `i2c` `None`), para o binário.

O laço (o que as assinaturas não determinam):

```
partida: root_error → status {false, root_error}; os métodos respondem 1002; nada roda
três tarefas juntas (tokio::join!) sob um supervise:

telas:
  descoberta(): choose + connected; para cada conector: barramento pelo link ddc, pelo cache ou,
    para os sem link, um match_edid por placa (read_edid de cada candidate_buses, num detached de op_timeout);
    i2c None → sem telas DDC; i2c_dev_loaded falso → a linha com o erro do i2c-dev;
    taken = os ddc + os do cache; sem conector novo nem EDID mudado → nenhuma leitura de i2c;
    monitor novo → thread própria (Decisões) + get_brightness → Adjuster; conector que saiu → thread encerrada, linha some;
    publica screens (ordem: a integrada, depois os conectores por nome)
  espera: assinante de screens, ou um método (que pede a descoberta e espera ela)
  com assinante: uevents.watch(); select! {
      uevent drm               → (re)arma o timer HOTPLUG_DELAY; ao disparar, descoberta()
      uevent backlight com devpath_name = o dispositivo → read_value; adjuster.external(v) → publica se true
      Lost                     → relê a integrada e descoberta() (nada de erro)
      método set/change(id)    → DDC com erro, max nulo ou leitura há mais de STALE: get_brightness antes (sucesso apaga o erro);
                                 adjuster.set/change → Step::Write(v) → escreve (writer ou a thread do monitor), segura WorkGuard
      resultado da escrita     → adjuster.written(lido) → Write(v) de novo | Publish(v) → publica, solta o guard | Failed(e) → erro na linha, solta o guard
      sem assinante            → larga o watch; métodos continuam atendidos sem uevent
  }
histórico: com assinante de history → bus.history() já, depois a cada history_interval → publica {points} ou {points: [], error}
perfis:    com assinante de profiles → bus.watch_profiles() → publica cada estado; sem assinante, larga o receptor
```

As escritas da integrada vão pelo `BacklightWriter` e, depois, a releitura é o `read_value` (o `TreeWriter` e o logind não devolvem o valor). Os métodos falam com o laço por um `mpsc` com resposta `oneshot`; o `{}` sai assim que o pedido entra no `Adjuster` (a releitura prévia do DDC, quando há, acontece depois da resposta). O `power.list` sempre chama a `descoberta()`.

- [ ] **Passo 1: os testes** (`tests/module.rs`, com os ajudantes do `tests/module.rs` do clipboard: `serve(cfg, work)` guardando o `ModuleTask`, `client(&path)`, `wait_until`, e `wait_seq`; `screens(&c) -> Vec<Value>`; `pct(&c, id)` = o `percent` da tela).
  - `handshake_lists_topics_and_methods`: `topics` `["power.screens", "power.history", "power.profiles", "power.status"]`, `methods` `["power.set", "power.change", "power.list"]`.
  - `params_are_validated`: um caso por texto de `-32602` do Formato (`set {"id":1,"percent":50}`, `{"id":"x".repeat(65),…}`, `{"id":"backlight","percent":0}`, `…101`, `…50.5`, `change {"id":"backlight","delta":101}`, `{"delta":"5"}`), cada um com a mensagem exata; `set {"id":"ddc:nada","percent":50}` → `1001` "Essa tela não existe.".
  - `screens_are_discovered`: com assinante, `power.screens` = a integrada (`output` `eDP-1`, `name` "Tela integrada", `value` 9600, `max` 19200, `percent` 50) e o LG (`ddc:HDMI-A-1`, `name` "LG ULTRAWIDE", `percent` 70), nessa ordem; `i2c.opened()` contém 0 e 1 (o EDID) e mais nada.
  - `set_goes_through_the_writer_and_rereads`: `set backlight 30` → `{}`; `writer.writes()` = `[("intel_backlight", 5760)]`; o tópico chega a `percent` 30.
  - `ddc_set_rereads_the_real_value`: o LG com `off_by: -1` → `set ddc:HDMI-A-1 40` → o tópico publica `value` 39.
  - `burst_of_changes_coalesces_on_a_slow_monitor`: `slow(1, 300 ms)`; 20 `change {delta: 5}` em 200 ms → `percent` final 100; as escritas de Set VCP em `writes(1)` são no máximo 3; entre o primeiro `change` e o fim, nenhum `power.screens` com o LG entre 70 e 100 exclusivo (o `wait_seq` guarda tudo o que chegou).
  - `dead_monitor_keeps_its_row_and_the_others_work`: um segundo monitor no barramento 0 (`card1-DP-1` falso); depois da descoberta, `hang(0)` e `set ddc:DP-1 30` → depois do `op_timeout` (300 ms no `fake_config`) a linha dele tem `error` "O monitor não respondeu ao DDC/CI." e continua na lista; `set backlight 20` e `set ddc:HDMI-A-1 50` chegam ao tópico em menos de 1 s.
  - `no_access_is_a_row_error_and_backlight_still_works`: `denied(1)` → o LG com `value` `null` e o `error` de acesso; a integrada ajusta.
  - `opener_never_sees_a_non_gpu_bus`: a árvore com `i2c-16` (SMBus) e `i2c-2` (DesignWare), um conector sem EDID casado → `opened()` só com 0 e 1, a linha com "Não achei o barramento deste monitor.".
  - `external_change_reaches_the_topic`: `tree.set_value("intel_backlight", 4800)` e `uevents.send("change", "backlight", "/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight")` → `percent` 25 em 500 ms; o mesmo com o devpath de outro dispositivo (`…/acpi_video0`) ou com `SUBSYSTEM` `drm` → nada muda.
  - `lost_events_reread_and_rediscover`: `tree.set_value("intel_backlight", 1920)` e `uevents.lose()` → `percent` 10 em 500 ms, o status continua `available`.
  - `hotplug_burst_rediscovers_once`: 10 `send("change", "drm", …)` em 50 ms, sem conector novo → `reads(0)` e `reads(1)` iguais aos de antes (nenhuma leitura de i2c); um conector novo `card1-DP-1` com outro EDID no barramento 0 → aparece, com leituras só no barramento 0; tirá-lo (`status` `disconnected` + uevent) → some.
  - `standby_monitor_recovers_on_the_next_set`: `busy_times(1, 4)` na descoberta → o LG com `error` "O monitor não respondeu ao DDC/CI." e `percent` `null`; depois `set ddc:HDMI-A-1 50` → um Get VCP antes do Set, a linha sem `error` e `percent` 50.
  - `stale_ddc_value_is_reread_before_change`: o LG lido em 70; `set_current(1, 30)` por fora; com `STALE` curto no `fake_config` (`stale` 100 ms: campo `pub stale: Duration` no `PowerConfig`) e 150 ms depois, `change {delta: 5}` → o Set VCP grava 35.
  - `uevent_only_with_subscriber`: sem assinante, `uevents.watching()` 0; assinando `power.screens`, 1; saindo, 0 em 1 s.
  - `methods_work_without_subscriber`: sem assinar, `power.list` → as duas telas; `set backlight 60` → `writer.writes()` com 11520.
  - `work_is_held_while_writing`: `slow(1, 300 ms)`, `set ddc:HDMI-A-1 10` → `work.pending()` 1 logo depois e 0 depois de publicado.
  - `writer_failure_is_a_row_error`: `writer.fails("Access denied")` → a integrada com `error` "Não consegui ajustar o brilho pelo logind: Access denied." e o `percent` de antes.
  - `history_only_with_subscriber`: sem assinante, `history_calls()` 0; com assinante, o tópico com os pontos e `history_calls()` ≥ 2 em 500 ms; `history_fails("sem UPower")` → `{"points":[],"error":"Não consegui ler o histórico do UPower: sem UPower."}`.
  - `profiles_follow_the_bus`: assinante → `{true, true}`; `set_profiles({false, false})` → publicado; sem assinante, `watching()` 0.
  - `history_error_is_clipped`: `history_fails(<5000 caracteres>)` → o `error` publicado com no máximo 300 + o texto fixo.
  - `bad_root_makes_the_module_unavailable`: `PowerConfig { root_error: Some(<texto>), … }` → `status` `{false, <texto>}`; `power.list` → `1002`; `opened()` vazio e nenhuma escrita.
  - `panic_marks_the_module_unavailable`: um `FakeBus` que entra em pânico no `history` (`FakeBus::panicking()`) com assinante → status com o erro interno; um cliente novo ainda faz o aperto de mão.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad-power --test module` → erro de compilação.
- [ ] **Passo 3: implementar** as Interfaces e o laço.
- [ ] **Passo 4: verificar.** `TMPDIR=/tmp cargo test -p lucernad-power --test module` → 23 passed, 5 vezes seguidas sem falha intermitente; o workspace inteiro verde; a sonda da Tarefa 4 de novo → 3 passed.
- [ ] **Passo 5: commit.** `git commit -m "Energia: o módulo power, com o juntar por tela e o plugar pelo uevent"`

---

### Tarefa 6: `lucernad` — o módulo no binário, a regra udev e o README

**Files:**
- Create: `crates/lucernad/data/60-lucernad-i2c.rules`, `crates/lucernad/data/lucernad-i2c.conf`, `crates/lucernad/tests/udev.rs`
- Modify: `crates/lucernad/{Cargo.toml,src/lib.rs,src/main.rs}`, `crates/lucernad/tests/{common/mod.rs,protocol.rs,process.rs}`, `README.md`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `lucernad_power::{register, PowerConfig, PowerTopics}`, `testing::{quiet_config, Tree}` (o `lucernad-power` com `testing` nas `[dev-dependencies]` do binário); `ServiceBuilder::work()`; `lucerna_cli::{CliCommand, Call}`.
- Produces:
  - `Cargo.toml`: `default = ["system", "spectrum", "clipboard", "power"]`, `power = ["dep:lucernad-power", "dep:lucernad-core", "dep:serde_json"]` (comentário `# O módulo power: o brilho das telas, o histórico de consumo e os perfis.`); `description` com "(system, spectrum, clipboard e power)".
  - `Modules` ganha `#[cfg(feature = "power")] pub power: PowerConfig` (`real()`: `PowerConfig::system()`); `Lucernad` ganha `power: PowerTopics` e `power_task: ModuleTask`; o `work()` do serviço passa também ao `register` do power (o mesmo `WorkTracker` do clipboard, clonado; com só o `power` ligado, ele pede o `work()`); as listas `any(feature = …)` ganham `"power"`; o `main` guarda `_power_task`; a descrição do `cli()` vira "O daemon do Lucerna: os dados do sistema, o espectro do som, o histórico da área de transferência e o brilho das telas para o shell.".
  - `fn plain(s: &str) -> String` sai do `cfg(feature = "clipboard")` (usado pelos dois módulos; `cfg(any(feature = "clipboard", feature = "power"))`).
  - Com `power`, o `CliCommand` `brilho` (`about` "Mostra as telas com brilho ajustável; com ID e PERCENT, ajusta uma (+N e -N somam ao valor atual)."; argumentos opcionais `id` e `percent`, este com `allow_hyphen_values`; só um dos dois → `Err("Passe o ID e o PERCENT juntos")`; `percent` que não seja `1`–`100` nem `+N`/`-N` com `N` de 1 a 100 → `Err("PERCENT precisa ser um inteiro entre 1 e 100, ou +N/-N com N até 100")`) → `power.list {}`, `power.set {id, percent}` ou `power.change {id, delta}`; `render` = a linha das Decisões por tela (pelo `plain()`), "Nenhuma tela com brilho ajustável." sem telas, nada para o `{}` do `set`.
  - `60-lucernad-i2c.rules`: o comentário (o lucernad ajusta o brilho dos monitores externos por DDC/CI; dá `uaccess` só aos barramentos i2c com um ancestral de classe PCI `0x03*`, as placas de vídeo; SMBus, sensores e touchpad ficam como estão; precisa de número menor que 73) e a linha `SUBSYSTEM=="i2c-dev", KERNEL=="i2c-[0-9]*", ATTRS{class}=="0x03*", TAG+="uaccess"`. `lucernad-i2c.conf`: `i2c-dev`.
  - CI: o clippy ganha `cargo clippy -p lucernad --no-default-features --features power --all-targets --locked -- -D warnings`.
  - `README.md`: "Situação" com os quatro módulos; em **Módulos**, a seção `power` no formato das outras (o que faz; quando roda; a tabela dos quatro tópicos; os métodos; a integrada pelo logind, os externos por DDC/CI, o i2c só das placas de vídeo; o `LUCERNAD_POWER_ROOT` dito como só de teste); em **Instalação**, os dois `sudo install -Dm644` (a regra em `/usr/lib/udev/rules.d/`, o `.conf` em `/usr/lib/modules-load.d/`), `sudo modprobe i2c-dev` e `sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev`, com uma frase sobre a regra do ddcutil ser equivalente; em **Linha de comando**, `lucernad brilho [ID PERCENT|+N|-N]`; em **Desenvolvimento**, a sonda do `tests/isolado.sh` com o `docker run` da Tarefa 4.

- [ ] **Passo 1: os testes.**
  - `tests/common/mod.rs`: `Modules { …, power: lucernad_power::testing::quiet_config(dir.path()) }`, guardando o quarto `ModuleTask`.
  - `protocol.rs`, `the_daemon_serves_the_power_module`: `hello().topics` contém os quatro `power.*`, `methods` contém `"power.list"`; `power.list {}` → `{"screens":[]}`.
  - `process.rs`: o `lucernad(dir)` passa a pôr `LUCERNAD_POWER_ROOT` = `dir/power` (uma `Tree` com `lucerna_teste`, `max` 1000, valor 500) e `DBUS_SYSTEM_BUS_ADDRESS=unix:path=<dir>/sem-barramento` em todo processo; em `cli_help_status_and_exit_codes`: `lucernad brilho` → código 0 e a linha `backlight  50%  Tela integrada`; `lucernad brilho backlight 80` → código 0 e, em 2 s, `dir/power/sys/class/backlight/lucerna_teste/brightness` = `800`; `lucernad brilho backlight` → código de uso, "Passe o ID e o PERCENT juntos"; `lucernad brilho backlight -10` → em 2 s o arquivo = `700`; `lucernad brilho backlight 0` → código de uso com "PERCENT precisa ser"; `lucernad brilho x 50` → código diferente de 0, "Essa tela não existe."; `brilho_lines_have_no_control_characters` (unidade em `lib.rs`, como a do `historico`): uma tela de nome `"a\u{1b}[2Jb"` e erro com `\u{202E}` → a linha sem caracteres de controle.
  - `tests/udev.rs`, `udev_rule_only_matches_gpu_classes`: as linhas do `data/60-lucernad-i2c.rules` que não são comentário nem vazias são exatamente a linha acima; `data/lucernad-i2c.conf` = `"i2c-dev\n"`.
- [ ] **Passo 2: rodar e ver falhar.** `cargo test -p lucernad` → falha.
- [ ] **Passo 3: implementar** as Interfaces.
- [ ] **Passo 4: verificar.** O workspace inteiro verde, com os seis clippy das Global Constraints; `grep -rnE "process::Command|tokio::process" crates/*/src` → nada; o `grep` do `PowerConfig::system|LinuxI2c|ZbusPower` das Global Constraints → só o `sonda.rs` (e o `lib.rs` do binário, no `Modules::real()`).
- [ ] **Passo 5: commit.** `git commit -m "lucernad: o módulo power no binário, o subcomando brilho, a regra udev e o README"`

---

### Tarefa 7: `lucernad` — conferência na máquina real, PR e merge

Roda depois das Tarefas 8–9. **Quem conduz é o agente, com o usuário presente**: cada passo que grava no hardware é anunciado antes, e o usuário confirma. Antes de começar, o controlador pede ao usuário: abrir a tampa do notebook (o `eDP-1` está `disabled` com ela fechada) e não mexer no brilho pelo Plasma durante os passos 3–5 (o PowerDevil do KDE também fala DDC com o LG).

**Files:** nenhum, salvo correções (cada uma com teste e commit próprio).

**Interfaces:**
- Consumes: o binário da Tarefa 6 (com a forma de transação da Tarefa 2a); o shell da `feat/energia-e-brilho` (Tarefas 8–9).
- Produces: `<LUCERNAD_REV>` = o SHA da `main` do `lucernad` depois do merge, para a Tarefa 10.

- [ ] **Passo 1: os valores originais e o daemon à parte.** Antes de tudo, anote: `ORIG_BL=$(cat /sys/class/backlight/intel_backlight/brightness)`; o valor do LG pela linha de comando do passo 3 (a primeira leitura). Daemon à parte do instalado:

  ```sh
  cargo build --release --locked -p lucernad
  R=$(mktemp -d /tmp/lucernad-conf.XXXXXX)
  run() { XDG_RUNTIME_DIR=$R LUCERNA_IDLE_TIMEOUT_MS=${IDLE:-600000} target/release/lucernad "$@"; }
  run serve & SERVE=$!; run watch power.screens > $R/telas.log & WATCH=$!
  ```

  Critério: `run brilho` lista `backlight` com o `percent` de `ORIG_BL` e `ddc:HDMI-A-1` "LG ULTRAWIDE" com um valor (achado pelo EDID, sem link `ddc`).
- [ ] **Passo 2: a tela integrada.** `run brilho backlight 50` → `cat …/brightness` = `raw_of(50, 19200)` = 9600 e o tópico com `percent` 50; a "tecla segurada": `for i in $(seq 20); do run brilho backlight -2 & done; wait` (20 `power.change` quase juntos) → valor final 10% e, no `telas.log`, bem menos valores da integrada entre o 50 e o 10 do que os 20 pedidos (o número que vier vai para o PR); **mudança de fora:** o usuário mexe no slider de brilho do Plasma → o `telas.log` acompanha em menos de 1 s. **Volta ao original:** `busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetBrightness ssu backlight intel_backlight $ORIG_BL` e `cat …/brightness` = `$ORIG_BL`.
- [ ] **Passo 3: o LG (DDC), com cuidado.** Anuncie cada escrita e espere o "ok" do usuário. (1) `run brilho` → anote `ORIG_LG` (`value` e `max`); (2) grave **o mesmo valor**: `run brilho ddc:HDMI-A-1 <percent de ORIG_LG>` → o relido igual a `ORIG_LG`, e o usuário confirma que nada mudou na tela; (3) um ajuste pequeno: `+5` (ou `−5` se `ORIG_LG` ≥ 96) → o relido bate e o usuário vê a mudança; (4) volte a `ORIG_LG` e confira o relido. Se o `max` não for 100 e o percentual não reproduzir o valor bruto, a volta é pelo `ddcutil --bus <n> setvcp 10 <ORIG_LG>` (ainda instalado) e a leitura confere. Critério: o valor final relido = `ORIG_LG`.
- [ ] **Passo 4: plugar e tirar.** O usuário tira o cabo do LG → em 3 s a linha some do `telas.log`; põe de volta → em 5 s volta com o valor (o EDID relido só dele). Se a NVIDIA não mandar uevent `drm` ao plugar, anote no PR e decida com o usuário (não é aceitável deixar sem: o plano B é reler os conectores a cada 5 s com assinante, como correção com teste).
- [ ] **Passo 5: as permissões.** Com o usuário, por `sudo`: instalar a regra e o `.conf` como no README, mascarar a do ddcutil só para o teste (`sudo ln -s /dev/null /etc/udev/rules.d/60-ddcutil-i2c.rules`), `udevadm control --reload`, `udevadm trigger --subsystem-match=i2c-dev`. Critério: `getfacl /dev/i2c-*` com `user:<usuário>:rw-` exatamente em `i2c-0`, `1` e `4`–`15` e **sem** em `i2c-2`, `i2c-3` e `i2c-16`; `run brilho` ainda ajusta o LG. Depois, `sudo rm /etc/udev/rules.d/60-ddcutil-i2c.rules` (o usuário decide se remove o pacote `ddcutil`), reload e trigger.
- [ ] **Passo 6: o shell, no Hyprland do container.** No `lucerna-shell` (branch `feat/energia-e-brilho`): `dev/test.sh up`; `docker cp target/release/lucernad lucerna-polish:/usr/bin/lucernad`; `dev/test.sh run 'pkill -x lucernad; sh ~/.config/quickshell/lucerna/ci/power-root.sh /tmp/lucerna-power && setsid -f env LUCERNAD_POWER_ROOT=/tmp/lucerna-power LUCERNA_IDLE_TIMEOUT_MS=86400000 lucernad serve'`; a conferência do `LUCERNAD_POWER_ROOT` pelo `/proc` (Global Constraints) **antes** de qualquer tecla; abrir a janela para o usuário. O usuário usa as teclas de brilho (segurando), o slider da central e o de Configurações → Tela e ociosidade, e abre a página da bateria (o gráfico vem do UPower do host, só leitura). Critério: o OSD e os sliders acompanham, o arquivo `/tmp/lucerna-power/…/brightness` do container muda, **o `brightness` do host não muda** (`cat` antes e depois = `$ORIG_BL`); `dev/test.sh errs` sem avisos novos; o usuário aprova.
- [ ] **Passo 7: PR.** `kill $WATCH $SERVE`; `rm -rf $R`; conferir de novo `ORIG_BL` e o LG; `git push -u origin feat/power`; `gh pr create --repo lucerna-desktop/lucernad --base main --title "lucernad: o módulo de energia e brilho" --body <resumo; dependência nova (i2cdev, sem unsafe no crate); como foi testado (a sonda do D-Bus isolada; a tabela dos passos 1–6 com o que se viu); as linhas "Para o PR" da tabela de decisões: o valor do brightness em vez do actual_brightness, o LUCERNAD_POWER_ROOT, o error do power.history, o power.list; as ACL antes e depois; o que o uevent da NVIDIA fez>`. **Controlador:** revisão final da branch (Opus), CI verde, `gh pr merge <n> --repo lucerna-desktop/lucernad --merge`; anote `<LUCERNAD_REV>` (`git fetch && git rev-parse origin/main`); com o usuário, reinstale o `/usr/bin/lucernad` (a memória "lucernad instalado no sistema") e a regra.

---

### Tarefa 8: `lucerna-shell` — `Brightness.qml` como adaptador

Repositório `lucerna-shell`, branch `feat/energia-e-brilho` (Global Constraints).

**Files:**
- Modify: `services/Brightness.qml` (reescrito), `tests/brightness.test.js` (reescrito), `tests/coverage.json`, `features/settings/state/IdleSettings.qml`, `features/settings/ui/IdlePage.qml:18-46`

**Interfaces:**
- Consumes: `Lucernad.client` (`want`, `call`, `connected`, `received`); o Formato.
- Produces (o singleton `Brightness`):
  - Propriedades: `property var client: Lucernad.client` (não `readonly`: os testes trocam); `property string error: ""`; `readonly property bool available: client.connected && error === ""`; `property var screens: []` (`[{ id, kind, label, output, value (0–1), error }]`, `value` 0 sem leitura); `readonly property real value` (o `value` da tela `kind === "backlight"`, 0 sem ela: o OSD a observa); `readonly property var focusedScreen: screens.find(s => s.id === focusedId()) ?? null`; `property string lastId: "backlight"`; `readonly property real shownValue: screens.find(s => s.id === lastId)?.value ?? value` (o que o OSD lê); `readonly property int holdMs: 1000`; `property var holds: ({})` (id → `Date.now()` do último ajuste local); `signal adjusted`.
  - Funções: `wantedTopics(): var` (`["power.status", "power.screens"]`); `sync(): void` (`client.want("power", …)`); `toScreen(s: var): var` (`label` = `s.name`, `value` = `s.percent / 100` ou 0, `error` = `s.error ?? ""`, `output` = `s.output ?? ""`); `merged(incoming: var, local: var, holds: var, now: real): var` (as telas que chegam, mas com o `value` local para os ids com ajuste há menos de `holdMs`); `apply(topic: string, data: var): void`; `applyScreens(data: var): void` (`Array.from(data.screens)` → `toScreen` → `merged`); `applyStatus(data: var): void`; `connectionChanged(connected: bool): void` (caída: `screens` `[]`, `error` ""; nos dois casos `sync()`); `withValue(list: var, id: string, value: real): var`; `set(fraction: real): void`; `setScreen(id: string, fraction: real): void` (limita a 0,01–1, `withValue`, `holds[id]`, `lastId`, `client.call("power.set", {id, percent: Math.round(f * 100)}, null)`, `adjusted()`); `focusedId(): string` (a tela sem erro cujo `output` é o `Hyprland.focusedMonitor?.name`; senão a `backlight` sem erro; senão a primeira sem erro; senão ""); `change(delta: real): void` (na tela em foco: `lastId` = o id dela, o valor local somado e limitado, `holds`, `client.call("power.change", {id, delta: Math.round(delta * 100)}, null)`, `adjusted()`).
  - Ligações: `Connections` no `client` (`onReceived` → `apply`, `onConnectedChanged` → `connectionChanged(client.connected)`); `Component.onCompleted: sync()`. O `IpcHandler` `brightness` (`up`, `down`, `set`, `setFor`, `list`) igual ao de hoje; o `list` mostra o erro no lugar do percentual.
  - Saem: `devMode`, `device`, `ddc`, `ddcAvailable`, `parse`, `refreshDdc`, os `Process` do `brightnessctl` e do `ddcutil`, o `ddcWriter`, o `Connections` do Hyprland e o `ddcDelay`.
  - Cabeçalho: "O brilho das telas, vindo do módulo power do lucernad: a tela integrada (pelo logind) e os monitores externos (por DDC/CI) com o valor real. As teclas ajustam a tela em foco; o daemon junta os ajustes. Sem o daemon, `screens` fica vazio."
  - `IdleSettings`: `brightnessUnavailable: string` (`""` com `Brightness.available`; senão `"Indisponível: sem conexão com o lucernad."` desconectado ou `` `Indisponível: ${Brightness.error}` ``); `IdlePage`: a seção "Brilho" visível com telas ou com o texto; o ícone pelo `kind`; a descrição = o `error` da tela, senão `` `${output} · pelo DDC/CI` `` para `ddc`; o `Slider` com `enabled: !screen.error`.
  - `tests/coverage.json`: `"services/Brightness.qml": ["wantedTopics", "sync", "toScreen", "merged", "apply", "applyScreens", "applyStatus", "connectionChanged", "withValue", "set", "setScreen", "focusedId", "change"]`.

- [ ] **Passo 1: os testes** (`tests/brightness.test.js`, cabeçalho dizendo o que testa; guarda e restaura `client`, `screens`, `error`, `lastId`, `holds`; `wired()`, `ok`, `ids`, `methods`, `last` como no `tests/clipboard.test.js`, com o hello oferecendo `power.screens` e `power.status`; `daemon(list)` = `{screens: list}` no formato do Formato).
  - `brilho: tópicos`: `wantedTopics()` = `["power.status", "power.screens"]`; com `wired()`, um `lucerna.subscribe` pendente com os dois.
  - `brilho: as telas pelo sinal real`: `C.client.received("power.screens", JSON.parse(JSON.stringify(daemon([integrada 50, LG 70, DP-1 com erro]))))` → `screens.length` 3, `screens[0]` `{id: "backlight", kind: "backlight", label: "Tela integrada", output: "eDP-1", value: 0.5, error: ""}`, `screens[2].error` o texto; `value` 0,5.
  - `brilho: a tela em foco`: sem monitor do Hyprland (o teste), `focusedId()` = `"backlight"`; com a integrada com erro → `"ddc:HDMI-A-1"`; com só a de erro → `""` e `focusedScreen` `null`.
  - `brilho: ajuste sai pelo fio e fica otimista`: `wired()` com as telas; `setScreen("ddc:HDMI-A-1", 0.4)` → um `power.set` pendente com `{id: "ddc:HDMI-A-1", percent: 40}`, `screens[1].value` 0,4 e o sinal `adjusted` (um `Connections` criado no teste conta); `setScreen("backlight", 0)` → `percent` 1.
  - `brilho: as teclas mandam o delta`: `lastId` posto em `"ddc:HDMI-A-1"`; `change(0.05)` → `power.change` com `{id: "backlight", delta: 5}`, `value` 0,55, `lastId` `"backlight"` e `shownValue` 0,55; `change(-2)` → `value` 0,01.
  - `brilho: o daemon não volta o slider no meio do arrasto`: `setScreen("ddc:HDMI-A-1", 0.4)` e, logo depois, o sinal com o LG em 70 → continua 0,4; `merged(…, holds com o LG há 2000 ms, …)` → 0,7.
  - `brilho: estado e queda`: `applyStatus({available: false, error: "x"})` → `available` falso; `{available: true, error: null}` com `wired()` → verdadeiro; `connectionChanged(false)` → `screens` `[]`.
  - `brilho: null e tópico desconhecido não mudam nada`; `brilho: estado original restaurado`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces e o `IdleSettings`/`IdlePage`.
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE brightness casos=9`; `grep -rn "brightnessctl\|ddcutil\|refreshDdc\|ddcAvailable" services features tests` → nada.
- [ ] **Passo 5: commit.** `git commit -m "Energia: Brightness lê do lucernad; saem o brightnessctl e o ddcutil"`

---

### Tarefa 9: `lucerna-shell` — o histórico e os perfis no `Battery.qml`, o gráfico e os textos

**Files:**
- Modify: `services/Battery.qml`, `features/central/state/PowerState.qml`, `features/central/ui/BatteryPage.qml:190-196`, `features/settings/ui/AboutPage.qml:76`, `tests/battery.test.js` (reescrito), `tests/coverage.json`, `README.md:147-149` e o que fala de `brightnessctl`/`ddcutil`/`busctl`, `Lucerna — Pendências.md:37,97` (viram `[x]` com "(feito em 2026-10-22: módulo `power` do `lucernad`)"), `Lucerna — Plugins Próprios.md:24,26` (as linhas do `lucerna-ddc` e do `lucerna-brightness` dizem que viraram o módulo `power`), `Lucerna — Proposta.md:31`

**Interfaces:**
- Consumes: `Lucernad.client`; o Formato.
- Produces:
  - `Battery`: `property var client: Lucernad.client`; `property string powerError: ""`; `readonly property bool powerAvailable: client.connected && powerError === ""`; `historyActive`, `history` (`[{time, value, state}]`, do mais antigo ao mais novo), `historyAt`, `historySpan` ficam; `property string historyError: ""`; `property bool profilesAvailable: false` (do `power.profiles` com conexão; sem conexão, fica com o último valor conhecido); `readonly property bool hasPerformance: PowerProfiles.hasPerformanceProfile` (fica o do Quickshell: decisão do controlador); funções `wantedTopics(active: bool): var` (`["power.status", "power.profiles"]`, mais `"power.history"` com `active`), `sync(): void` (`client.want("battery", …)`), `apply(topic: string, data: var): void`, `applyHistory(data: var): void` (`Array.from(data.points)`, só pontos com `time` e `rate` números e `rate` ≥ 0, `{time, value: rate, state}`, ordenados; `historyAt` = `Date.now() / 1000`; `historyError` = `data.error ?? ""`), `applyProfiles(data: var): void` (só o `available`), `applyStatus(data: var): void`, `connectionChanged(connected: bool): void` (caída: `history` `[]`, `powerError` "", o `profilesAvailable` **fica**; `sync()`); `onHistoryActiveChanged: sync()`; o `Connections` no `client`; `Component.onCompleted: sync()`.
  - Saem: `parseHistory`, `pushSample`, `historyAfter`, `upowerHistory`, `historyFailures`, `historyProbed`, `probeHistory`, o `onRealBatteryChanged`, os dois `Timer` e os dois `Process` (o `historyQuery` e a sonda dos perfis). Fica o resto (bateria, `setProfile`, o modo simulado, o IPC).
  - `PowerState`: `readonly property string chartNote` (`""` com `chart.length > 0`; desconectado → `"Indisponível: sem conexão com o lucernad."`; com `Battery.powerError` → `` `Indisponível: ${Battery.powerError}` ``; com `Battery.historyError` → `` `Indisponível: ${Battery.historyError}` ``; senão `"Medindo"`); o `BatteryPage` mostra o `chartNote` no lugar do "Medindo" fixo, `visible` com ele não vazio.
  - `AboutPage`: `"PipeWire, UPower e NetworkManager"`.
  - `tests/coverage.json`: `"services/Battery.qml": ["wantedTopics", "sync", "apply", "applyHistory", "applyProfiles", "applyStatus", "connectionChanged"]`.

- [ ] **Passo 1: os testes** (`tests/battery.test.js`; guarda e restaura `client`, `historyActive`, `history`, `historyAt`, `historyError`, `profilesAvailable`, `powerError`; `wired()` com o hello oferecendo os quatro `power.*`).
  - `bateria: tópicos conforme a página`: `wantedTopics(false)` / `(true)`; com `wired()`, ligar `historyActive` põe um `lucerna.subscribe` de `power.history` pendente.
  - `bateria: o histórico pelo sinal real`: `C.client.received("power.history", JSON.parse(JSON.stringify({points: [{time: 1, rate: 5, state: 2}, {time: 2, rate: 7.5, state: 1}]})))` → `history` = `[{time: 1, value: 5, state: 2}, {time: 2, value: 7.5, state: 1}]`, `historyAt` > 0; pontos sem número ou com taxa negativa ficam de fora; `{points: [], error: "e"}` → `historyError` `"e"`.
  - `bateria: perfis do daemon`: o sinal com `{available: true, performance: false}` → `profilesAvailable` verdadeiro; o `hasPerformance` continua igual ao `PowerProfiles.hasPerformanceProfile`.
  - `bateria: queda não esconde os perfis`: `connectionChanged(false)` → `history` `[]`, `profilesAvailable` continua verdadeiro; com `{available: false, …}` pelo sinal e conectado → falso.
  - `bateria: o gráfico diz por que não há pontos` (pelo `t.PowerState`, com `history` `[]`): desconectado → `"Indisponível: sem conexão com o lucernad."`; `wired()` e `historyError` `"x"` → `"Indisponível: x"`; sem erro → `"Medindo"`.
  - `bateria: null e tópico desconhecido não mudam nada`; `bateria: estado original restaurado`.
- [ ] **Passo 2: rodar e ver falhar.** `dev/ci.sh unit` → falha.
- [ ] **Passo 3: implementar** as Interfaces e a documentação (o README sem o `brightnessctl` e o `ddcutil` nas dependências, com o `lucernad` dando o brilho, o gráfico e os perfis; as notas).
- [ ] **Passo 4: verificar.** `dev/ci.sh lint rules unit` → verde, `SUITE battery casos=7`; `grep -rn "busctl\|brightnessctl\|ddcutil" services features tests README.md` → nada.
- [ ] **Passo 5: commit.** `git commit -m "Energia: o histórico e os perfis vêm do lucernad; sai o busctl"`

---

### Tarefa 10: `lucerna-shell` — o `lucernad` novo no container e na fumaça, PR e merge

**Files:**
- Create: `ci/power-root.sh`
- Modify: `dev/lucernad.rev` (`<LUCERNAD_REV>`), `dev/Dockerfile:25` (saem `brightnessctl` e `ddcutil`), `dev/hyprland.lua` (o `lucernad` sobe com a raiz falsa), `dev/test.sh:37-39` (o exemplo `setsid -f lucernad serve` do cabeçalho passa a `sh ~/.config/quickshell/lucerna/ci/power-root.sh /tmp/lucerna-power && setsid -f env LUCERNAD_POWER_ROOT=/tmp/lucerna-power lucernad serve`, com a frase do porquê) e o roteiro (a conferência do `LUCERNAD_POWER_ROOT`), `ci/smoke.sh`, `ci/tolerated.txt:21-28`, `ci/tests/test_logcheck.py:31-34`

**Interfaces:**
- Consumes: `<LUCERNAD_REV>` (Tarefa 7); `lucernad --json brilho`.
- Produces:
  - `ci/power-root.sh <pasta>` (`set -euo pipefail`; cria a árvore das Decisões; recusa pasta que não seja absoluta ou que comece por `/sys`, `/dev` ou `/proc`).
  - `dev/hyprland.lua`: dentro do `case` de hoje, `sh ~/.config/quickshell/lucerna/ci/power-root.sh /tmp/lucerna-power && exec env LUCERNAD_POWER_ROOT=/tmp/lucerna-power LUCERNA_IDLE_TIMEOUT_MS=86400000 lucernad serve`, com o comentário do porquê (o D-Bus do sistema do host está montado; sem a raiz falsa, o brilho do host mudaria).
  - `ci/smoke.sh`: o `lucernad serve` sobe com `LUCERNAD_POWER_ROOT=$XDG_RUNTIME_DIR/power` (criada pelo `power-root.sh` antes); a fase "brilho", depois da fase da área de transferência: em 5 s, `lucernad --json brilho` com a tela `backlight` em 50 (`row "brilho" "tela integrada falsa em 5 s" …`); `qs … ipc call brightness up` → o `brightness` da árvore = 550 em 2 s, e a captura `NN-osd-brilho.png` (`row … "tecla"`); 10 `ipc call brightness up` sem espera → 1000 em 2 s (`row … "rajada"`); a central aberta com o slider (a captura dela). Na fase "lucernad fora", Configurações → Tela e ociosidade capturada com o "Indisponível". O cabeçalho do script ganha a fase "brilho" e as reprovações dela.
  - `ci/tolerated.txt`: sai a linha do `brightnessctl` e o comentário dela passa a falar só do `hyprctl`; avisos novos só entram medidos, cada bloco com o motivo, linha inteira. `ci/tests/test_logcheck.py`: sai o exemplo do `brightnessctl`.

- [ ] **Passo 1: a revisão e o container.** Gravar `<LUCERNAD_REV>`; o `power-root.sh`, o `Dockerfile`, o `hyprland.lua` e o roteiro do `test.sh`. Verificar: `dev/test.sh up` reconstrói a imagem; `dev/test.sh run 'command -v brightnessctl ddcutil'` → nada; a conferência do `/proc/<pid>/environ` (Global Constraints) mostra `LUCERNAD_POWER_ROOT=/tmp/lucerna-power`; `dev/test.sh run 'lucernad --json brilho'` → só a `backlight` falsa; um `lucernad serve` sem a raiz (`dev/test.sh run 'env -u LUCERNAD_POWER_ROOT lucernad --json status'` contra um daemon de teste subido assim num socket próprio) → `power.status` com o erro "Em container…"; `dev/test.sh ipc brightness up` → o arquivo do container muda e o `cat /sys/class/backlight/intel_backlight/brightness` do host fica igual.
- [ ] **Passo 2: a fumaça.** `smoke.sh`, `tolerated.txt` e `test_logcheck.py`. Verificar: `dev/ci.sh smoke` → verde, com as linhas da fase "brilho" no `ci-out/smoke/summary.md` e as capturas do OSD, da central e da página de ociosidade indisponível; `dev/ci.sh` (os quatro) → verde.
- [ ] **Passo 3: commit, PR e merge.** `git commit -m "Energia: o lucernad novo no container e na fumaça, sem o brightnessctl e o ddcutil"`; `git push -u origin feat/energia-e-brilho`; `gh pr create --repo lucerna-desktop/lucerna-shell --base main --title "Energia e brilho: o shell lê do lucernad" --body <resumo; a revisão fixa do lucernad; como foi testado (unidade pelo fio do cliente e pelo sinal real, a fumaça com a tela falsa, a conferência da Tarefa 7); as capturas; o LUCERNAD_POWER_ROOT no container e na fumaça>`. **Controlador:** revisão final da branch (Opus), CI verde, `gh pr merge <n> --repo lucerna-desktop/lucerna-shell --merge`.

---

## Autorrevisão

**Cobertura do spec:** leitura da integrada pelo sysfs com a ordem do kernel (Tarefa 1); escrita pelo logind na sessão gráfica, sem root (Tarefas 4 e 7); ajustes juntados, uma escrita em voo, mínimo de 1% (Tarefas 3 e 5); mudanças de fora com assinante (Tarefas 3, 5 e 7); descoberta pelos conectores `connected` com o nome do conector como id e sem o eDP (Tarefa 1); o link `ddc` e a comparação de EDID na mesma placa, também ao plugar (Tarefas 1, 5 e 7); DDC/CI próprio com 0x37, checksum, atrasos e VCP 0x10 com o máximo do monitor, `i2cdev` sem `unsafe` no crate (Tarefa 2); fila por monitor, juntar, até 3 novas tentativas, releitura publicada (Tarefas 2, 3 e 5); monitor que não responde fica com o erro e não trava os outros (Tarefa 5); plugar e tirar pelo uevent com atraso (Tarefas 3, 5 e 7); a regra udev só das placas de vídeo e o `i2c-dev` no README, o erro claro sem permissão (Tarefas 5, 6 e 7); histórico pelo `GetHistory("rate", 3600, 120)` com assinante, a cada 60 s, com o estado de cada ponto (Tarefas 4 e 5); perfis acompanhados, inclusive o serviço que sobe depois (Tarefas 4 e 5); os quatro tópicos, os dois métodos e o `-32602` em pt-BR (Formato, Tarefa 5); o adaptador do brilho com a API de hoje, o histórico e os perfis no `Battery`, o que sai do shell, o "Indisponível", a fumaça com tela falsa sem tocar no host (Tarefas 8–10); os testes Rust com sysfs, i2c, logind e D-Bus falsos e a máquina real (Tarefas 1–7). Sem lacunas.

**Passos:** cada passo de código traz as assinaturas, os textos e os números; o único algoritmo escrito é o laço, que as assinaturas não determinam; os bytes do DDC estão nos testes, conferidos por conta.

**Tipos:** `Roots`, `root_from_env`, `Backlight`, `choose`, `read_value`, `Connector`, `connected`, `candidate_buses`, `is_gpu_bus`, `edid_name`, `BusMatch`, `match_edid`, `Tree` (Tarefa 1); `I2cBus`, `I2cOpener`, `LinuxI2c`, `DdcTiming`, `Vcp`, `DdcError`, `get_brightness`, `set_brightness`, `read_edid`, `FakeI2c`, `FakeMonitor` (Tarefa 2); `Adjuster`, `Step`, `raw_of`, `percent_of`, `Uevent`, `Uevents`, `KernelUevents`, `FakeUevents` (Tarefa 3); `PowerBus`, `BacklightWriter`, `Point`, `Profiles`, `ZbusPower`, `LogindWriter`, `TreeWriter`, `FakeBus`, `FakeWriter`, `isolated` (Tarefa 4); `PowerConfig`, `register(…, WorkTracker)`, `fake_config`, `quiet_config` (Tarefas 5–6) batem entre si; no shell, `screens`/`value`/`focusedScreen`/`setScreen`/`change`/`adjusted` mantêm os nomes que o `ControlsState`, o `OsdState`, o `InputState` e o `IdleSettings` usam, e `history`/`historyAt`/`historySpan`/`profilesAvailable`/`hasPerformance` os que o `PowerState` e o `SettingsState` usam.

**Revisão do plano (aplicada):** a trava do container no daemon, o `TreeWriter` preso à raiz, os candidatos sem os barramentos do cache, a releitura antes de escrever num monitor com erro ou velho, as unidades do `Adjuster`, o `flock`, a Tarefa 2a, o `& 0x7F`, os perfis sem o lucernad, o `ENOBUFS`, o aviso da partida, a comparação por componente, a variável vazia, o casamento pelo `DEVPATH`, o `shownValue`/`lastId`, o `power.list` sempre redescobrindo, o corte dos erros do D-Bus e a thread presa anotada.

**Review Focus:** os cinco itens têm teste na tarefa dona (1, 2, 3 e 5) e os itens 3 e 4 também a conferência da Tarefa 7.

**Proporção:** maior que o spec porque cobre dois repositórios, a sonda, a fumaça e a conferência; o código fica nas assinaturas, e os blocos de código são o laço, o formato e os comandos que precisam ser exatos.
