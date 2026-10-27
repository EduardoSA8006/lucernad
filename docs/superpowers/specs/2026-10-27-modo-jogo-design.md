# lucernad: o modo jogo (e o começo do módulo `hyprland`)

**Data:** 2026-10-27
**Base:** os specs anteriores do `lucernad` (o daemon, o `lucernad-core`, a conexão permanente do shell, o cliente com donos e filtro pelo `lucerna.hello`, "disponível" = conectado e sem erro, zbus com prazo, falsos na feature `testing`, testes do shell pelo caminho real do sinal e da resposta) e o spec do desfoque do shell (`lucerna-shell/docs/superpowers/specs/2026-10-27-desfoque-desempenho-design.md`, PR #57): as regras que o shell manda ao Hyprland, o `reapply()` depois de um reload, a pausa do papel de parede.
**Repositórios:** `lucernad` (os módulos novos `hyprland` e `game`, e o "modo leve" nos módulos `system`, `spectrum` e `clipboard`) e `lucerna-shell` (o adaptador `services/GameMode.qml`, a parte do shell e a página "Modo jogo"). O `lucerna-sdk` não muda.
**Hyprland:** 0.56.2 com config em Lua (conferido no container `lucerna-polish` em 2026-10-27).

## Por quê

O monitor do usuário é de 144 Hz, e os efeitos do Hyprland (desfoque, sombras, animações) e o que o shell faz por conta própria (papel de parede animado, popups, a barra que desce pelo topo, a coleta do `lucernad`) custam quadros e atrapalham num jogo. Hoje só há remendos: o papel pausa em tela cheia, a ociosidade segura em tela cheia. O modo jogo junta tudo num estado só, com entrada automática, desfaz exatamente o que fez ao sair e sobrevive à queda do shell ou do daemon.

## Decisões do usuário

- **Pulou as perguntas** e aprovou o pacote inteiro ("vamos incluir tudo", 2026-10-27):
  - **Entrada:** automática por tela cheia de verdade; automática por jogo detectado (Steam/Proton com `SteamAppId`, gamescope, Wine, Lutris); manual, por uma chave na barra de ações rápidas do Painel e por um atalho, e **o que entrou manual só sai manual**; lista por app (sempre / nunca).
  - **Compositor, pelo IPC do Hyprland:** desfoque, sombras e animações desligados e **restaurados exatamente como estavam**; VRR na janela em tela cheia; tearing opcional, **desligado por padrão**.
  - **Shell:** papel de parede animado pausado; não perturbe automático, com um resumo das notificações ao sair; a barra não se revela pelo topo, mas o atalho ainda a revela; ociosidade inibida.
  - **Sistema:** perfil de energia "desempenho", voltando ao anterior ao sair; o próprio `lucernad` mais leve (coleta do sistema mais espaçada, espectro pausado, histórico da área de transferência adiado); integração com o GameMode da Feral, se instalado.
  - **Saída e visibilidade:** ícone discreto e aviso ao entrar e ao sair; restauração garantida mesmo se o shell ou o daemon caírem; página "Modo jogo" nas Configurações, com cada item.
- **Sem reserva**, como nos outros módulos: sem o `lucernad`, o modo jogo fica indisponível (a chave apagada, a página com o aviso).

## Decisões do assistente

Tomadas porque o usuário pulou as perguntas; cada uma pode ser revista sem mudar o resto do desenho.

1. **Módulo `hyprland` mínimo.** O `lucernad` ganha o crate `lucernad-hyprland` só com o que o modo jogo usa: achar a instância, os pedidos pelo `.socket.sock` (`j/getoption`, `j/clients`, `j/monitors`, `eval`, `setprop`) e os eventos pelo `.socket2.sock`. Ele já nasce organizado para receber depois os atalhos, os monitores e a entrada (um arquivo por assunto, o cliente de socket e os eventos compartilhados), mas **o módulo Hyprland completo (item 7 das Pendências) não entra agora**.
2. **O `lucernad` é a fonte da verdade** do estado do modo jogo (tópico `game.state`). O shell manda a configuração (`game.configure`) e os pedidos manuais (`game.set`/`game.toggle`) e **deriva** a parte dele do tópico; nada do modo jogo é gravado nas preferências do shell (o não perturbe do usuário continua dele).
3. **Mudanças no Hyprland por `eval` com `hl.config({...})`**, como o shell já faz (o Lucerna exige config em Lua). Valores lidos por `j/getoption` (o formato conferido: `{"option", "bool"|"int"|"float"|"str", "set"}`).
4. **O modo jogo só mexe em opções que o shell nunca mexe:** `decoration:blur:enabled`, `decoration:shadow:enabled`, `animations:enabled`, `misc:vrr` e `general:allow_tearing`. O `reapply()` do `ThemeSwitcherState` (tamanho, passadas, `new_optimizations`, `popups`, `debug.vfr`, regras de camada) não toca nelas, então os dois não brigam depois de um reload. O anti-objetivo do spec do desfoque ("nada de desligar `decoration.blur.enabled`") vale para o shell; no modo jogo, desligar é o pedido.
5. **Restauração exata, chave por chave:** antes de mudar, guarda o valor lido de cada opção; ao sair, só devolve a opção que ainda está com o valor que o modo jogo pôs (se o usuário mudou no meio, a mudança dele vence). Depois de um `configreloaded` durante o modo jogo, os valores recarregados viram o novo "antes" e o modo jogo reaplica por cima.
6. **VRR:** com `misc:vrr` em 0, passa a 2 (só em tela cheia) e volta a 0; com 1, 2 ou 3, não mexe (nunca reduz o que o usuário escolheu).
7. **Tearing (desligado por padrão):** ligado nas Configurações, põe `general:allow_tearing = true` e `setprop address:0x<janela> immediate 1` nas janelas que dispararam o modo; ao sair, `immediate 0` nas que ainda existem e o `allow_tearing` de antes. Com o `allow_tearing` restaurado em falso, um `immediate` que tenha sobrado não tem efeito.
8. **Gatilhos:**
   - **tela cheia de verdade** = uma janela com o bit 2 do `fullscreen` (o mesmo critério do `Hypr.hasRealFullscreen` do shell) num workspace à mostra em algum monitor; entra depois de 1 s estável (os outros gatilhos entram na hora);
   - **jogo detectado** = uma janela cujo processo (ou um ancestral, até 8 níveis) tem `SteamAppId` ou `SteamGameId` não vazio e diferente de `0`, ou é o `gamescope`, ou é Wine (executável `wine*-preloader`/`wine`/`wine64`, ou `argv[0]` terminado em `.exe`), ou roda sob o `lutris-wrapper`; vale **enquanto a janela existir**, em qualquer workspace e com ou sem foco (sair do jogo com Alt+Tab não liga e desliga os efeitos);
   - **sempre** = existe uma janela com a classe da lista; **nunca** = janelas com a classe da lista não contam para tela cheia nem para jogo detectado. A classe é comparada sem distinguir maiúsculas, inteira;
   - a **saída automática** espera 3 s sem nenhum gatilho, para todos (não pisca numa troca rápida nem num jogo que recria a janela).
9. **Manual:** `game.set {on: true}` liga e prende ligado (os gatilhos não desligam). `game.set {on: false}` desliga e, se houver gatilho automático presente, **prende desligado** até todos os gatilhos sumirem (desligar na mão com o jogo aberto não religa no segundo seguinte). A trava de manual se perde se o daemon reiniciar (o estado volta ao automático).
10. **Perfil de energia pelo `HoldProfile("performance", …)`** do power-profiles-daemon (`org.freedesktop.UPower.PowerProfiles`, com o `net.hadess.PowerProfiles` como reserva), na conexão do sistema do próprio módulo, e `ReleaseProfile` ao sair. O daemon de perfis solta a retenção sozinho se a conexão cair, e solta se o usuário trocar o perfil na mão: a volta ao perfil anterior é dele, não nossa.
11. **GameMode da Feral:** com o `com.feralinteractive.GameMode` no barramento da sessão, `RegisterGameByPID(<pid do lucernad>, <pid do jogo>)` para cada processo de janela que disparou o modo (o de tela cheia, o detectado, o da lista "sempre") e `UnregisterGameByPID` ao sair ou quando a janela fecha. Modo manual sem janela que dispare não registra nada. Sem o GameMode, o item aparece como "não instalado".
12. **`lucernad` mais leve** por um sinal interno (`lucernad_core::GameSignal`, um `watch<bool>` ligado no binário): o `system` amostra no mínimo a cada 5 s; o `spectrum` fecha a sessão do PipeWire e publica `paused: true` no `spectrum.status`; o `clipboard` continua lendo as cópias (a seleção de um app some se não for lida na hora), mas guarda até 50 delas na memória sem miniatura, sem banco e sem publicar, e as processa em ordem ao sair (as mais velhas caem além de 50, com um aviso no log). Cada um pode ser desligado pela página.
13. **Restauração garantida, em camadas:**
    - um **diário** (`$XDG_RUNTIME_DIR/lucerna/game/restore.json`, pasta 0700) com a assinatura da instância do Hyprland, os valores de antes e os que o modo jogo pôs, gravado **antes** de qualquer mudança no compositor e apagado depois da restauração;
    - ao **sair limpo** (o serviço parando), o módulo restaura antes de o processo terminar;
    - o unit ganha `ExecStopPost=/usr/bin/lucernad restaurar-jogo`, um subcomando escondido que lê o diário e fala direto com o socket do Hyprland, sem daemon (cobre o `SIGKILL` e a queda);
    - ao **subir**, o módulo restaura o diário que achar antes de qualquer outra coisa;
    - diário de **outra instância** do Hyprland (o Hyprland reiniciou e já leu a config do zero) é apagado sem restaurar;
    - enquanto ativo, o módulo segura um `WorkGuard`: o daemon não sai pelo "60 s sem cliente" com o shell caído e os efeitos desligados;
    - o perfil e o GameMode se soltam sozinhos com a queda do processo (decisões 10 e 11);
    - a parte do shell é derivada do tópico, então o shell reiniciado volta ao certo, e a queda do daemon (conexão perdida) **desliga a parte do shell na hora**.
14. **Shell:** o não perturbe do modo jogo é um segundo interruptor que não grava nada (`Notifications.doNotDisturb` = o do usuário **ou** o do modo jogo). O resumo ao sair é calculado das notificações ainda na lista com `receivedAt ≥ since` (o `since` vem do tópico), então sobrevive a um reinício do shell; ele aparece como um cartão próprio na pilha de popups ("Enquanto você jogava: 4 notificações — Discord, Steam"), que abre o painel de notificações ao clicar, e não aparece com zero. O aviso de entrar e sair é um OSD novo (tipo `game`). O ícone é um controle (`sports_esports`) na área de status da barra, à esquerda do sino; clicar desliga (pelo `game.set`).
15. **Atalho:** uma ação nova do shell, `game` ("Ligar/desligar o modo jogo"), na lista de Atalhos (grupo "Painéis e sessão"), sem tecla padrão; e uma ação nova `bar` ("Mostrar/esconder a barra"), também sem tecla padrão, que revela a barra na tela com foco (fora e dentro do modo jogo) e a esconde no segundo toque ou como a revelação pelo mouse. Pelo terminal: `lucernad jogo [ligar|desligar|alternar]` e `qs -c lucerna ipc call game toggle|on|off|get`.
16. **Configuração antes do shell:** até o primeiro `game.configure`, só a entrada manual funciona (com os itens padrão); a automática espera o shell dizer o que quer.
17. **Padrões de fábrica:** todos os itens ligados, menos o tearing; lista "sempre" vazia; lista "nunca" com os players e navegadores comuns (D24).

Da conferência do construtor (releitura do spec sem poder perguntar):

18. **Restauração que falha** (socket do Hyprland sem resposta): o diário fica, o erro vai ao `effects.compositor` e ao log, e o módulo tenta de novo a cada 5 s e a cada evento do Hyprland enquanto houver diário; o `WorkGuard` só é solto com o diário apagado.
19. **Avisos só de mudança vista:** o shell não mostra o OSD de entrar/sair nem o resumo pelo primeiro estado depois de (re)conectar; só por uma troca de `active` com a conexão de pé. A queda do daemon desliga a parte do shell em silêncio.
20. **O resumo conta** as notificações não críticas ainda na lista com `receivedAt ≥ since` (as críticas já apareceram como popup), agrupadas pelo nome do app (até três nomes, depois "e mais N").
21. **`spectrum.status` ganha `paused`** (falso fora do modo leve); o adaptador do shell só ignora o campo (as barras param, como sem som). O `system` e o `clipboard` não mudam de formato.
22. **O `lucernad` sem Hyprland** (o PC do usuário em KDE, a fumaça no `sway`): o `hyprland.status` fica indisponível com o motivo, o `game` funciona sem gatilhos automáticos e sem o compositor (`effects.compositor`, `vrr` e `tearing` = `unavailable`), e a entrada manual ainda aplica o perfil, o GameMode, o modo leve e a parte do shell.

Das duas perguntas que ficaram em aberto (o usuário pediu para pular; respondidas pelo assistente em 2026-10-27):

23. **Jogo fora de foco:** um jogo detectado mantém o modo ligado **enquanto a janela existir**, mesmo fora de foco ou em outro workspace (reforça a D8). Só a tela cheia depende de a janela estar à mostra.
24. **Vídeo em tela cheia também liga o modo** (a tela cheia não distingue jogo de vídeo), mas a lista "nunca" **já vem preenchida** com os players e navegadores comuns, e o usuário pode tirar qualquer item dela. A lista de fábrica (classes do Hyprland, comparadas inteiras e sem distinguir maiúsculas): `mpv`, `vlc`, `io.github.celluloid_player.celluloid`, `org.gnome.totem`, `org.kde.haruna`, `smplayer`, `firefox`, `firefox-esr`, `librewolf`, `floorp`, `zen`, `zen-browser`, `chromium`, `google-chrome`, `brave-browser`, `vivaldi-stable`, `microsoft-edge`, `opera`. No `game.configure`, `never` ausente = essa lista; `never: []` = lista vazia (o usuário tirou tudo). A lista de fábrica sai do `lucernad` (`config::DEFAULT_NEVER`), e o shell a mostra como ponto de partida na página.

## Objetivos

- Um estado único do modo jogo no `lucernad`, com motivo (manual, tela cheia, jogo, lista), publicado ao shell.
- Entrada e saída automáticas sem piscar, e manual com a regra "manual só sai manual".
- Desfoque, sombras e animações desligados, VRR em tela cheia e (opcional) tearing, e **tudo de volta exatamente como estava**, inclusive depois de um reload, da queda do shell e da queda do daemon.
- Perfil de desempenho e GameMode enquanto ativo, soltos ao sair.
- O `lucernad` gastando menos enquanto ativo.
- No shell: papel parado, não perturbe com resumo, barra presa (menos pelo atalho), ociosidade segurada, ícone e avisos, a chave no Painel, o atalho e a página nas Configurações.
- Medição: o custo do Hyprland e do `lucernad` com e sem o modo, e a prova de que as opções voltam iguais.

## Não objetivos

- O módulo Hyprland completo (atalhos, monitores, entrada, `xkbcli`) e a troca do `hyprctl eval` do shell pelo socket: ficam no item 7 das Pendências.
- Overlay de FPS, limitador de quadros, troca de resolução ou de taxa do monitor.
- Mexer no agendador, na CPU ou na GPU por conta própria (isso é do GameMode).
- Gravar no `hyprland.lua` do usuário ou em qualquer arquivo de config do Hyprland.
- Detectar jogos que não se encaixam nos sinais da decisão 8 (para eles, a lista "sempre" ou a tela cheia).
- Outros compositores.

## Anti-objetivos

- **Deixar a sessão do usuário com efeitos desligados**, em qualquer caminho: saída normal, saída manual, reload no meio, shell caído, daemon caído ou morto, Hyprland reiniciado. É o anti-objetivo central.
- Sobrescrever uma mudança que o usuário fez durante o modo jogo (ele ligou as animações na mão: ao sair, ficam ligadas).
- Reduzir o VRR que o usuário escolheu, ligar o tearing sem ele pedir.
- Gravar o não perturbe, a pausa ou qualquer coisa do modo jogo nas preferências do shell (a queda nunca deixa o usuário em "não perturbe" sem saber).
- Ligar e desligar os efeitos a cada Alt+Tab, troca de workspace ou vídeo que pisca em tela cheia.
- Perder uma cópia da área de transferência feita durante o jogo (dentro do limite de 50).
- Ler o ambiente ou a linha de comando de processos de outros usuários, mandar qualquer coisa para fora da máquina ou guardar o ambiente lido (só o "é jogo / não é").
- Qualquer teste que toque o Hyprland real, o D-Bus real, hardware real ou a internet.

## Restrições

- O usuário hoje **não usa Hyprland no PC real** (o host é KDE): toda conferência visual e de restauração é no container `lucerna-polish` (Hyprland 0.56.2 aninhado, GPU Intel, 60 Hz), com uma cópia do `lucernad` fixada pelo `dev/lucernad.rev` do shell. A fumaça do CI roda no `sway` headless, sem Hyprland: lá o módulo `hyprland` fica indisponível, e a fase do modo jogo usa um Hyprland falso (`ci/hypr-fake.py`, só no ambiente do `lucernad`).
- Prazos: todo pedido ao socket do Hyprland com prazo (2 s); toda chamada D-Bus com o `DBUS_TIMEOUT` da casa; nada disso trava o laço.
- Sem processo externo (`hyprctl`, `gamemoded`, `powerprofilesctl`) em `crates/*/src`.
- `unsafe_code = "deny"`; dependências novas só no `[workspace.dependencies]`.
- Valores atuais conferidos no container: `decoration:blur:enabled` true, `decoration:shadow:enabled` true, `animations:enabled` true, `misc:vrr` 0, `general:allow_tearing` false; `hyprctl` tem `eval`, `getoption`, `setprop` e `getprop`.

## Arquitetura

### Crate `lucernad-hyprland` (feature `hyprland`, padrão)

- **Instância:** `HYPRLAND_INSTANCE_SIGNATURE` do ambiente; sem ela (o serviço do systemd nem sempre a herda), a pasta mais nova em `$XDG_RUNTIME_DIR/hypr/` cujo `.socket.sock` responde. Refeita quando o socket some.
- **Pedidos** (`.socket.sock`, uma conexão por pedido, resposta até o EOF, prazo e limite de tamanho): `getoption`, `clients`, `monitors`, `eval`, `setprop`, `version`.
- **Eventos** (`.socket2.sock`, linhas `nome>>dados`): um canal de difusão para quem assinar; reconecta com espera crescente; publica `hyprland.status` (`{available, instance, version, error}`).
- **Atrás de um trait** (`Hypr`), com o real por socket e o `FakeHypr` na `testing` (respostas roteirizadas, pedidos gravados, eventos injetados); mais um servidor de sockets falso em pasta temporária para testar o cliente de verdade.
- Organização: `instance`, `socket` (pedido e resposta), `events`, `options` (ler/escrever opções e o Lua do `hl.config`), `clients` (janelas e monitores). Os futuros `binds`, `monitors` e `input` entram ao lado.

### Crate `lucernad-game` (feature `game`, padrão; depende de `hyprland`)

- **Detecção** (`detect`): a partir de `j/clients` e `j/monitors`, recalculada (com 300 ms de junção) nos eventos `openwindow`, `closewindow`, `fullscreen`, `workspacev2`, `movewindowv2`, `activewindowv2`, `monitoradded`, `monitorremoved` e `configreloaded`; o "é jogo" de cada pid lido uma vez do `/proc` (de uma raiz trocável, como no `system`) e guardado até o pid sumir.
- **Decisão** (`state`): função pura dos gatilhos, da configuração e da trava manual → ativo ou não, motivos e janelas.
- **Efeitos** (`effects`): compositor, VRR, tearing, perfil, GameMode e o modo leve, cada um com o seu resultado (`ok`, `off`, `unavailable`, ou o erro), aplicados nessa ordem e desfeitos ao contrário; o diário antes do compositor.
- **Tópicos e métodos:**
  - `game.state` — `{active, manual: "on"|"off"|null, reasons: ["manual"|"fullscreen"|"game"|"always"], windows: [{address, class, title, reason}], since (ms desde a época, ou null), effects: {compositor, vrr, tearing, profile, gamemode, light}}`;
  - `game.status` — `{available, error}` (indisponível só se o módulo parou; sem Hyprland, o modo jogo funciona sem a parte do compositor, e o `effects` diz `unavailable`);
  - `game.set {on}` → `{}`; `game.toggle {}` → `{active}`;
  - `game.configure {auto: {fullscreen, games}, always: [classe], never: [classe], compositor: {effects, vrr, tearing}, system: {profile, gamemode, light}}` → `{}` (substitui tudo; ausente = padrão; até 100 classes por lista, de até 128 caracteres).
  - Erros de parâmetro com o `-32602` do SDK e texto em pt-BR.
- **Linha de comando:** `lucernad jogo [ligar|desligar|alternar]` (sem argumento mostra o estado) e o escondido `lucernad restaurar-jogo` (fora do `lucerna-cli`, tratado no `main` antes dele).

### Modo leve nos módulos existentes

O binário cria o `GameSignal` e o passa ao `system`, ao `spectrum` e ao `clipboard` (sem a feature `game`, um sinal sempre falso). O `game` liga o sinal só com o item `light` ligado.

### Lado do shell (`lucerna-shell`)

- **`services/GameMode.qml`**, adaptador do `game.state`/`game.status` (dono `"game"`): `available`, `active`, `manual`, `reasons`, `since`, `effects`; `set(on)`, `toggle()`; o `game.configure` inteiro a cada conexão e a cada mudança das preferências. Com a conexão caída, `active` vira falso na hora.
- **Preferências** no `Config` (`gameAuto*`, `gameAlways`, `gameNever`, `gameEffects`, `gameVrr`, `gameTearing`, `gameProfile`, `gameGamemode`, `gameLight`, e os do shell: `gameWallpaper`, `gameQuiet`, `gameBar`, `gameIdle`, `gameNotices`).
- **Aplicação:** `WallpaperState.shouldAnimate` falso com `GameMode.active && gameWallpaper`; o `Notifications.doNotDisturb` do serviço = `Config.doNotDisturb || (GameMode.active && gameQuiet)`; `BarState` ignora a revelação pelo mouse e a espiada da troca de workspace com `GameMode.active && gameBar` (a ação `bar` continua); `IdleState.held` ganha o modo jogo (com o motivo "O modo jogo está ligado"); o OSD `game` e o cartão de resumo com `gameNotices`.
- **Painel:** a chave "Modo jogo" no `OverviewState.quickToggles` (apagada sem o `lucernad`).
- **Configurações → Modo jogo** (tópico novo `game`): o estado agora (com os motivos e o efeito de cada item, inclusive "não instalado"/"indisponível"); Entrada (tela cheia, jogos detectados, o atalho com link para Atalhos); Apps (listas sempre/nunca, com o acréscimo pelas janelas abertas e pelo nome da classe); Compositor (efeitos, VRR, tearing com o aviso de que pode rasgar a imagem); Shell (papel, não perturbe e resumo, barra, ociosidade, avisos); Sistema (perfil, GameMode, `lucernad` leve).
- **Camadas:** a lógica fica nos `state/` e no serviço; os `ui/` não importam `qs.services` (o `ci/rules.py`).

## O que fica para o construtor

- As bibliotecas (nenhuma nova é esperada: `tokio::net::UnixStream`, `zbus`, `serde_json`, `rustix`) e a organização interna dos crates.
- Os textos da interface e das mensagens, no tom das Configurações.
- O desenho do cartão de resumo e do OSD `game`, dentro do visual dos atuais.
- O formato exato do diário, desde que traga a instância, o antes e o depois de cada opção.
- A sintaxe exata do `setprop` para o `immediate` (conferir com `hyprctl setprop --help` e um `getprop` no container).
- O texto do `README` (o `ExecStopPost`, o `HYPRLAND_INSTANCE_SIGNATURE` no ambiente do systemd, o GameMode opcional).

## Testes

- **Rust (nada real):** o parse do `getoption`, `clients`, `monitors` e dos eventos; o cliente de socket contra o servidor falso em pasta temporária (prazo, resposta enorme, socket que some, reconexão dos eventos); a detecção sobre um `/proc` falso (Steam com e sem `SteamAppId`, `SteamAppId=0`, Proton, gamescope, Wine, Lutris pelo ancestral, processo de outro dono); a decisão (manual só sai manual, trava desligada até os gatilhos sumirem, sempre/nunca, a espera de 1 s e 3 s com o relógio do `tokio` pausado); a restauração (valor mudado pelo usuário não é sobrescrito, reload no meio refaz o "antes", VRR nunca reduzido, tearing só ligado na configuração); o diário (gravado antes do `eval`, restaurado na subida, de outra instância apagado, o `restaurar-jogo` sem daemon); o perfil e o GameMode com D-Bus falso (ausente, erro, prazo); o modo leve no `system`, no `spectrum` e no `clipboard` (o limite de 50 e a ordem). Prova de que nada sai: a suíte inteira no `unshare -rn`, como no módulo `web`.
- **Shell:** o adaptador pelo caminho real (conexão, `configure`, estado, queda da conexão desligando tudo); as regras do papel, do não perturbe (sem gravar o `Config`), da barra, da ociosidade e do resumo (a partir do `since`); a chave do Painel e as ações `game`/`bar`; cobertura de 100% do escopo; fumaça sem avisos, com a fase "modo jogo" (o Hyprland falso confere que o `eval` desligou e restaurou, e o IPC `game get` do shell acompanha).
- **Container `lucerna-polish`:** entrar e sair por tela cheia (um `kitty` em tela cheia), pelo manual e pela lista; reload no meio; `kill -9` no `lucernad` com o modo ativo e as opções de volta depois; `qs` morto e reiniciado com o modo ativo; tudo conferido por `hyprctl -j getoption` antes e depois (iguais).

## Medição

- **Custo:** no container, o script de medição do desfoque (`dev/bench-blur.sh` como base) ganha uma irmã `dev/bench-game.sh` no shell: com um `kitty` em tela cheia rodando um laço que escreve a cada 16 ms e o papel animado, mede a CPU do Hyprland, do `qs` e do `lucernad` com o modo desligado e ligado (`lucernad jogo ligar/desligar`), e devolve o estado de antes. Esperado: Hyprland e `lucernad` claramente abaixo com o modo ligado. O usuário pode rodar o mesmo script numa sessão Hyprland de verdade quando tiver uma.
- **Restauração:** o mesmo script (e a fumaça) compara o `getoption` das cinco opções antes de ligar e depois de desligar; qualquer diferença reprova.

## Pendências da revisão final (lucernad)

Ficaram de fora das correções da revisão (2026-10-27); nenhuma deixa os efeitos desligados no caso comum.

- **M1, corrida do reload:** o `apply_after_reload` toma os valores lidos como o novo "antes" (D5). Se um `reconcile` comum roda entre o reload e o `configreloaded` (um `configure` ou uma troca das janelas do tearing nesse intervalo de milissegundos), ou se o `configreloaded` se perde num `Lagged`, os valores lidos são os do modo jogo, e ao sair os efeitos ficam desligados. A correção simples (no modo fresco, manter o `before` do diário quando `cur == applied && before != applied`) troca a D5: um arquivo editado para `blur = false` durante o jogo voltaria a `true` ao sair. É uma decisão do usuário.
- **M2, reaplicação por cima da mudança do usuário no meio do modo:** um `game.configure` ou uma troca das janelas do tearing desliga de novo o que o usuário ligou durante o jogo. Ao sair, o resultado está certo. Ideia: guardar as opções "devolvidas ao usuário" e não replanejá-las até o modo sair.
- **M3, restauração pendente que pisa numa escolha posterior:** com a restauração em nova tentativa, se o usuário (ou um reload) põe a opção no mesmo valor que o modo jogo tinha posto, a nova tentativa a devolve ao `before`. É raro.
- **M8, texto da D11 e do README:** o GameMode limpa o registro pelo pid do **jogo**, não do chamador. Com o `lucernad` morto e o jogo vivo, o registro fica até o jogo fechar. É inofensivo, mas o texto "se soltam sozinhos com a queda do processo" está impreciso para o GameMode. Os pids ≤ 0 já não são enviados.
- **M9, `immediate` depois do reload:** no modo fresco, as janelas já marcadas não recebem o `immediate 1` de novo. Se o reload zera as propriedades das janelas (não conferido no 0.56.2), o tearing não volta nelas.
- **M10, horário das cópias adiadas:** as cópias do modo jogo entram no histórico com o horário da descarga (`now + i`), não o da cópia. A ordem fica certa, mas o "copiado há X" mente. Basta guardar o `now_ms()` no `defer`.
- **M7, resto:** uma falha `Unreachable` no `j/clients` (o socket sumiu) ainda vale como "sem gatilhos" e solta a trava "desligado na mão"; só os soluços (prazo, resposta estranha) mantêm os gatilhos de antes.

