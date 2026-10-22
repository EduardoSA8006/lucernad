# lucernad: o módulo de energia e brilho

**Data:** 2026-10-22
**Base:** os specs anteriores do `lucernad` (`2026-10-20-sistema-design.md`, `2026-10-20-espectro-design.md`, `2026-10-21-area-de-transferencia-design.md`): o daemon, o `lucernad-core`, a conexão permanente do shell, o cliente com donos e filtro pelo `lucerna.hello`, o "sem reserva".
**Repositórios:** `lucernad` (o módulo `power`) e `lucerna-shell` (os serviços `Brightness.qml` e `Battery.qml` viram adaptadores nas partes que usam ferramenta externa). O `lucerna-sdk` não muda, salvo se o módulo provar que precisa.

## Por quê

O shell controla o brilho rodando um `brightnessctl` por ajuste (sem fila: segurar a tecla dispara vários processos que se atropelam) e o brilho dos monitores externos com o `ddcutil` (sonda tudo ou nada, sem nova tentativa, e a escrita não relê o valor real). Na máquina do usuário o `brightnessctl` nem está instalado, então a tela do notebook não é controlada. O histórico de consumo do gráfico e a checagem de perfis de energia usam o `busctl` por `sh -c`. O shell é só a interface: o que hoje depende de ferramenta externa vai para código próprio em Rust.

## Decisões do usuário

- Pulou as perguntas e aprovou as recomendações do construtor do documento (2026-10-22), abaixo.
- **Sem reserva**, como nos outros módulos: saem o `brightnessctl`, o `ddcutil` e o `busctl` do shell; sem o `lucernad`, o brilho e o gráfico ficam indisponíveis.

## Objetivos

- Brilho da tela integrada sem processo externo e sem root, com ajustes juntados (segurar a tecla não dispara uma escrita por toque) e mudanças de fora refletidas no shell.
- Brilho dos monitores externos por DDC/CI em código próprio, com fila por monitor, nova tentativa e releitura do valor real.
- Monitores plugados e tirados percebidos pelo próprio `lucernad`.
- Histórico de consumo e perfis disponíveis por D-Bus em código próprio.
- A interface igual à de hoje: slider da central, teclas de brilho e OSD, página da bateria e gráfico, brilho por tela na ociosidade.

## Não objetivos

- Mudar como o shell lê a bateria e troca o perfil: isso já é nativo do Quickshell (`Quickshell.Services.UPower` e `PowerProfiles`) e fica no shell.
- Gravar histórico de bateria próprio (o UPower já guarda).
- Outros controles DDC além do brilho (contraste, entrada, volume do monitor).
- Brilho automático por sensor de luz.

## Anti-objetivos

- Trocar uma ferramenta externa por outra (nada de chamar `brightnessctl`, `ddcutil`, `busctl` ou `light` por baixo).
- Exigir root ou setuid para o brilho.
- Acesso a barramentos i2c que não sejam de placa de vídeo.
- Travar o shell ou o laço do daemon com um monitor lento ou que não responde.

## O módulo `power`

### Tela integrada (backlight)

- **Leitura** direto do sysfs (`/sys/class/backlight/<dispositivo>`: `brightness`, `actual_brightness`, `max_brightness`); escolha do dispositivo pela ordem do kernel (`firmware`, depois `platform`, depois `raw`), como o `brightnessctl`.
- **Escrita pelo logind:** `org.freedesktop.login1.Session.SetBrightness("backlight", <dispositivo>, <valor>)` na sessão do usuário, por D-Bus em código próprio (ex.: `zbus`, que o `lucernad` já usa). Não precisa de root nem de regra udev. Como o `lucernad` roda como serviço de usuário (fora da sessão), a sessão é achada pelo `login1` (a sessão gráfica ativa do usuário) (decisão do construtor do documento).
- **Ajustes juntados:** só o último valor pedido é escrito, com no máximo uma escrita em voo; mínimo de 1% (como hoje).
- **Mudanças de fora** (tecla do firmware, outro programa): refletidas no tópico, observando o `actual_brightness` (inotify ou udev; escolha do construtor) enquanto houver assinante.

### Monitores externos (DDC/CI)

- **Descoberta** pelos conectores DRM `connected` (`/sys/class/drm/card*-<conector>/status`); o nome do conector (ex.: `HDMI-A-1`) é o id que o shell casa com o monitor do Hyprland. O eDP (tela integrada) fica de fora do DDC.
  - Com o link `ddc` no conector (Intel e AMD), o barramento é esse.
  - Sem o link (o driver da NVIDIA não o cria; na máquina do usuário o LG está no `card1-HDMI-A-1` da NVIDIA, cujos barramentos são `i2c-0`/`i2c-1`), o barramento sai da **comparação de EDID**: ler o EDID (endereço 0x50) dos barramentos i2c da mesma placa de vídeo e casar com o `/sys/class/drm/<conector>/edid`. Vale também ao plugar (decisão do construtor do documento, conferida na máquina em 2026-10-22).
- **Protocolo DDC/CI** em código próprio sobre `/dev/i2c-N` (endereço 0x37, checksum, os atrasos que o padrão exige entre comandos), lendo e gravando o VCP `0x10` (brilho, com o máximo que o monitor informa). Biblioteca de baixo nível de i2c permitida (ex.: `i2cdev`/`linux-embedded-hal`), desde que sem processo externo e sem `unsafe` no crate do módulo, se possível.
- **Fila por monitor:** uma operação por vez no mesmo barramento; ajustes juntados (só o último); nova tentativa (até 3) quando o monitor não responde; depois de gravar, relê o valor e publica o real.
- **Monitor que não responde** não some da lista nem trava os outros: fica com o estado e o erro dele.
- **Plugar e tirar:** o `lucernad` observa os eventos de DRM pelo udev/netlink (em código próprio) e refaz a descoberta, com um atraso curto para o monitor acordar.

### Permissão de i2c

- O `lucernad` instala a própria regra udev (ex.: `60-lucernad-i2c.rules`) que dá acesso (`uaccess`) aos barramentos i2c **das placas de vídeo** (dispositivo pai de classe PCI `0x03*`), como faz a regra do pacote do `ddcutil` hoje — a única forma de alcançar os barramentos da NVIDIA, que não estão ligados a um conector. Barramentos de outros dispositivos (sensores, SMBus da placa-mãe) ficam de fora (decisão do construtor do documento). A regra vai no README de instalação (com `sudo`) e, mais tarde, no pacote do Arch. O módulo `i2c-dev` precisa estar carregado (o README diz como).
- Sem permissão, o monitor aparece com o erro claro ("sem acesso ao barramento do monitor"), e a tela integrada continua funcionando.

### Energia

- **Histórico de consumo:** `org.freedesktop.UPower.Device.GetHistory("rate", 3600, 120)` da bateria do notebook, por D-Bus em código próprio, enquanto houver assinante do tópico (o shell assina só com a página da bateria aberta, como hoje); a cada 60 s.
- **Perfis disponíveis:** lidos e acompanhados pelo D-Bus (`net.hadess.PowerProfiles` ou `org.freedesktop.UPower.PowerProfiles`), inclusive se o serviço subir ou cair depois (fecha a pendência da sonda que só rodava na partida). Ler e trocar o perfil continua no shell pelo Quickshell.

### Tópicos e métodos

- `power.screens` — `{screens: [{id, kind: "backlight"|"ddc", output?, name, value, max, percent, error?}]}`: as telas com brilho controlável (a integrada e os monitores DDC), com o valor real.
- `power.history` — `{points: [{time, rate, state}]}`: o histórico de consumo com o estado de carga de cada ponto, que o gráfico de hoje usa para separar carregando e descarregando (vazio sem bateria) (decisão do construtor do documento).
- `power.profiles` — `{available: bool, performance: bool}`: o serviço de perfis existe e se tem o perfil de desempenho.
- `power.status` — `{available, error}`.
- `power.set {id, percent}` → `{}`: ajusta o brilho de uma tela (juntado); `power.change {id, delta}` → `{}`: soma ao valor atual (para as teclas).
- Erros de parâmetro com o `-32602` do SDK e texto em pt-BR.

## Lado do shell (`lucerna-shell`)

- **`services/Brightness.qml` vira adaptador** do `power.screens` (dono `"power"`), mantendo a API de hoje para quem usa (o `focusedId`/`focusedScreen` que casa com o `Hyprland.focusedMonitor` pelo `output`, `change`, `set`, `setFor`, o IPC `brightness`, os sinais que o OSD usa).
- **`services/Battery.qml`:** o histórico vem do `power.history` (assinado com a página da bateria aberta) e o `profilesAvailable`/`hasPerformanceProfile` do `power.profiles`; o resto (bateria e troca de perfil pelo Quickshell, o modo dev simulado) fica igual. Sai o recurso de amostras próprias (`pushSample`) e o `historyAfter`, porque não há reserva.
- **Sai do shell:** o `brightnessctl`, o `ddcutil` e os `busctl` (e o `sh -c` deles), as menções no README, no `dev/Dockerfile`, no `ci/tolerated.txt` e no `AboutPage`; o evento de monitor do Hyprland usado só para refazer a sonda de DDC.
- **Sem o `lucernad`:** o slider de brilho e o gráfico ficam indisponíveis (o padrão "Indisponível" do core); a bateria e o perfil continuam, porque vêm do Quickshell.
- **Teste e CI:** a fumaça exercita o brilho com uma tela integrada falsa (sysfs falso ou dublê) e sem monitor DDC; nada mexe no brilho do host.

## Testes

- **Rust:** a escolha do dispositivo de backlight e a leitura do sysfs com árvores falsas; o juntar de ajustes e a escrita pelo logind (dublê); o protocolo DDC/CI (mensagens, checksum, respostas inválidas, nova tentativa) contra um i2c falso; a descoberta pelos conectores com árvore DRM falsa; o histórico e os perfis com D-Bus falso; "só com assinante".
- **Máquina real:** o brilho da tela do notebook muda pelas teclas e pelo slider, inclusive segurando a tecla; o do LG (HDMI-A-1, na NVIDIA, achado pelo EDID) também, e o valor relido bate; plugar/tirar o monitor; o gráfico da bateria e os perfis; nenhum barramento i2c fora das placas de vídeo fica acessível pela regra nova.
- **Shell:** os adaptadores pelo caminho real do sinal e da resposta (lição do espectro), fumaça sem avisos, cobertura 100%.

## O que fica para o construtor

- As bibliotecas (i2c, udev/netlink, inotify) e a organização interna.
- Os atrasos e prazos do DDC, o atraso depois de plugar, o passo das teclas no `change`.
- A forma exata da regra udev, desde que limitada aos barramentos de monitor.
- Como a fumaça simula a tela integrada.
