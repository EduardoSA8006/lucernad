# lucernad: o módulo de espectro

**Data:** 2026-10-20
**Base:** `docs/superpowers/specs/2026-10-20-sistema-design.md` (o daemon, a conexão permanente do shell, o cliente do protocolo em QML e o "sem reserva"), já implementado e mesclado (`lucernad` #5, `lucerna-shell` #45).
**Repositórios:** `lucernad` (o módulo `spectrum`) e `lucerna-shell` (o adaptador `Spectrum.qml` e a saída do `cava`). O `lucerna-sdk` não muda, salvo se o módulo provar que precisa.

## Por quê

As barras e ondas em volta da capa (card de mídia da aba Painel e aba Mídia) vêm hoje do `cava`, um processo externo genérico: 32 barras a 30 quadros por segundo, sem faixa de frequência escolhida, sem suavização no shell (cada barra salta para o valor novo a cada quadro) e escutando a saída de som inteira, então qualquer som mexe nas barras. O shell é só a interface; a análise vai para código próprio em Rust, com controle total do comportamento, e prepara o terreno para um equalizador.

## Decisões do usuário

- **O visual fica como está**; o trabalho é aprimorar o movimento — mais suave e mais detalhado. O resto do como ficou a cargo do construtor do documento ("deixo a seu gosto").
- **Só o player exibido:** as barras mostram só o som do player que o card está mostrando (`Media.active` do shell). Spotify selecionado → só o Spotify; trocou para o Brave → só o Brave.
- **Sem reserva**, como no módulo de sistema: sai o `cava` e sai o modo de reserva pelo nível do som (`Audio.peak`). Sem o `lucernad`, o anel fica parado.
- Aprovou o desenho abaixo (2026-10-20).

## Objetivos

- Capturar o áudio do player exibido pelo PipeWire, em código próprio, sem processo externo.
- Movimento fluido: 60 quadros por segundo, bandas em escala que segue o ouvido, subida rápida e queda suave, ganho automático lento.
- Mais detalhe com o mesmo desenho dos anéis.
- Só trabalhar com alguém olhando: anel visível e player tocando.

## Não objetivos

- O equalizador (fica para outro projeto; aqui só a separação que ele vai aproveitar).
- Detecção de batida.
- Mudar o desenho dos anéis (`SpectrumRing.qml`) além do número de barras.

## Anti-objetivos

- Trocar o `cava` por outra ferramenta externa (nada de `pw-record`, `pw-cat` ou `cava` por baixo).
- Mexer no som do usuário: a captura só lê; não muda volume, links de saída, sink padrão nem roteamento.
- Barras mexendo com som de outro app que não o player exibido.
- O daemon capturando com o anel fechado ou o player parado.

## O módulo `spectrum`

### Captura

- **PipeWire direto do Rust** (biblioteca nativa do PipeWire, por exemplo o crate `pipewire`; a escolha é do construtor), num fluxo de captura ligado **ao nó de saída do player** (`Stream/Output/Audio`), não ao monitor do sink. Formato float, mono (média dos canais), na taxa do nó.
- **Achar o nó do player** a partir do nome do player no D-Bus (`org.mpris.MediaPlayer2.*`, mandado pelo shell):
  1. PID do dono do nome no D-Bus do usuário (`GetConnectionUnixProcessID`, por biblioteca de D-Bus em Rust, ex. `zbus`).
  2. Para cada nó `Stream/Output/Audio`, o PID está no **client** do nó (`client.id` → `application.process.id`), não nas propriedades do nó.
  3. O nó é do player se o PID do client é o do dono **ou um descendente dele** (subindo os `ppid` em `/proc`).
  4. **Ponte:** quando o dono é uma ponte de MPRIS conhecida e não o app, a busca parte do **pai** da ponte. As pontes ficam numa **lista fixa** pelo nome do executável (hoje só `plasma-browser-integration-host`, que é filho do navegador); nunca uma regra genérica de "tenta o pai", que casaria quase todos os fluxos quando o pai é o `systemd --user` ou o compositor (decisão do construtor do documento). Com o Brave no KDE: dono 120143 (`plasma-browser-integration`), pai 119549 (`brave`), áudio no 152224, filho de 119549 — conferido na máquina do usuário.
  5. **Reserva:** sem casamento por PID, comparar `application.process.binary` do client com o nome do player (o final do nome D-Bus sem o `.instanceNNNN`, ou o `DesktopEntry` do MPRIS), **só com igualdade exata e só se todos os nós que casam vierem de um mesmo processo**; em dúvida, nenhum nó (anel parado é melhor que barras de outro app) (decisão do construtor do documento).
  6. Mais de um nó casando: somar todos (um app com vários fluxos, ex. abas do navegador tocando juntas).
- **Acompanhar mudanças:** nós aparecem e somem (o Chromium só cria o fluxo com a aba fora do mudo; pausar pode suspender ou tirar o nó). O módulo observa o registro do PipeWire e religa a captura quando o nó do player aparece, sem o shell pedir de novo. Sem nó, o anel cai para zero pela queda suave e o módulo fica esperando.
- Casos conferidos na máquina do usuário (Arch, PipeWire 1.6.9, KDE): Spotify nativo (dono 153543 = PID do client do nó 89, `node.name` `spotify`); Brave pela ponte do Plasma. No Hyprland o Brave registra o próprio nome MPRIS (`brave.instanceNNNN`), caso que a regra 3 cobre.

### Análise

- **FFT própria** (por exemplo `rustfft`), janela de Hann, tamanho e sobreposição escolhidos pelo construtor para dar resolução nos graves e **60 quadros por segundo** de saída.
- **40 bandas** (ajustado olhando o anel com o usuário em 2026-10-20: eram 48) de ~45 Hz (eram ~30 Hz, no mesmo ajuste) a ~14 kHz (eram ~16 kHz) em escala que segue o ouvido (logarítmica ou mel; escolha do construtor), juntando os bins de cada banda; as bandas abaixo de 100 Hz usam uma FFT de 8192 amostras (bin próprio para cada uma, como o cava), as outras uma de 4096. Nessas bandas graves a faixa mostrada é de 36 dB abaixo da referência (`BASS_RANGE_DB`; as outras, 54 dB), para o bumbo saltar, e cada uma fica com pelo menos 50% da vizinha de cima (`BASS_BORROW`), para não abrir buraco quando a música não tem sub-grave (ajustado olhando o anel com o usuário em 2026-10-21).
- **Suavização no daemon:** subida rápida (ataque) e queda suave com aceleração (gravidade); valores finais em 0..1. As constantes são do construtor, ajustadas olhando o anel com música real.
- **Ganho automático lento**, para uma música baixa ainda mexer bem e uma alta não colar no teto; sem bombear entre faixas.
- **Separação para o equalizador:** captura, análise e publicação em partes separadas, com a análise como função pura (amostras → bandas) testável sem PipeWire.

### Regras de comportamento

- **Player parado:** quem decide é o shell, como hoje: o `active` do `Spectrum.qml` já exige `Media.playing` (`DashboardState.qml`), então com o player pausado a assinatura de `spectrum.bands` cai e a captura para. O daemon não acompanha o `PlaybackStatus` do MPRIS; um nó que continua aberto depois da pausa não importa, porque sem assinante nada roda (decisão do construtor do documento).
- **Só com assinante e com player:** a captura roda enquanto houver assinante de `spectrum.bands` **e** um player configurado; sem um dos dois, nada roda (nem fluxo do PipeWire aberto).
- **Trocar de player** (`spectrum.configure`) fecha a captura anterior e liga a nova; vale o último pedido.
- **Nada trava o laço:** o PipeWire roda na thread dele; o D-Bus e o `/proc` têm prazo; um player que some ou um PipeWire que cai viram estado, não pânico.
- **Não mexer no som:** os fluxos de captura não podem virar saída nem mudar links existentes; o player continua tocando como antes.

### Tópicos e métodos

- `spectrum.bands` — tópico contínuo (o "só o último" do `lucerna-ipc`), `{bands: [40 números 0..1]}`, a 60 por segundo enquanto captura; ao parar, um último quadro com zeros (o anel desce). Números com 3 casas.
- `spectrum.status` — `{available: bool, capturing: bool, player: string?, error: string?}`: módulo disponível (PipeWire alcançável), capturando agora, o player atual e o erro.
- `spectrum.configure {player: string|null}` → `{}`: o nome D-Bus do player exibido (ou `null` para nenhum).

## Lado do shell (`lucerna-shell`)

- **`services/Spectrum.qml` vira adaptador** do `spectrum.bands`, pelo cliente do protocolo que já existe (`services/Lucernad.qml`, `services/lucerna/LucernaClient.qml`). Mantém o papel de hoje para quem consome: `active` (quem mostra pede), `available` (daemon conectado e `spectrum.status.available`), `bars` (agora 40; eram 48, ajustado olhando o anel com o usuário em 2026-10-20) e `values` (o último quadro). `active` liga e desliga a assinatura de `spectrum.bands`.
- **O player exibido:** o adaptador manda `spectrum.configure {player}` com o nome D-Bus de `Media.active` (o `MprisPlayer` do Quickshell; o construtor confirma a propriedade do nome D-Bus) sempre que ele muda, e `null` sem player; reenvia ao reconectar.
- **Mais detalhe (decisão do construtor do documento):** card de mídia com 40 barras; aba Mídia espelhada com 80 (as mesmas 40 bandas nas duas metades, como hoje com 32 → 64). Ajustado olhando o anel com o usuário em 2026-10-20: o desenho começou com 48/96; 32 ficou pouco e 40 foi o meio-termo escolhido. Se ficar apertado no tamanho do card, voltar aos números de hoje é aceitável e deve ir no PR.
- **Renderização:** o daemon já entrega suavizado a 60 por segundo; o `SpectrumRing.qml` desenha direto, sem `Behavior` por barra. Uma interpolação no QML entre quadros só entra se a fumaça ou o olho mostrarem engasgo.
- **Sai do shell:** `services/spectrum/Cava.qml`, a config gerada, o `setpriv`, o `command -v cava`, as funções `configFor`/`parse`/`shape`/`failuresAfter`/`retryDelay` e os testes e entradas no `tests/coverage.json` correspondentes; o modo de reserva por `Audio.peak` no `DashboardState.qml` (e o `Audio.monitorPeak`, se ninguém mais usa); o `cava` do `dev/Dockerfile`; as menções no README (dependência opcional) e nas Pendências (a linha do espectro e a de que o `cava` escuta a saída inteira).
- **Sem o `lucernad`** ou com o módulo indisponível: o anel fica parado no tamanho de repouso, sem aviso extra na interface.
- **Ambiente de teste e CI:** a fumaça exercita o anel com o `lucernad` de pé. No container não há player real: o construtor escolhe como testar (por exemplo um player MPRIS falso com um fluxo do PipeWire tocando ruído **num sink nulo**, nunca nos alto-falantes do host).

## Testes

- **Rust:** a análise como função pura (senoide numa frequência acende a banda certa; silêncio dá zero; ruído branco espalha; o ganho automático sobe e desce devagar; ataque e queda nas constantes); o casamento player → nó com dados de exemplo (PID igual, descendente, ponte do Plasma, reserva por nome, vários nós, nenhum nó); "só com assinante e com player"; troca de player; nó que some e volta. PipeWire e D-Bus atrás de interfaces com dublês nos testes.
- **Máquina real:** com Spotify e Brave tocando, o anel segue só o exibido e troca ao trocar o player; uma notificação ou outro app tocando não mexe nas barras; o player continua tocando igual; o `lucernad` não deixa fluxo aberto com o anel fechado. Sem tocar nada nos alto-falantes por conta própria: o usuário põe o que tocar.
- **Shell:** o adaptador (assina com `active`, manda o player ao mudar e ao reconectar, `values` vazio parado), a fumaça sem avisos, cobertura 100%.
- CI dos dois repositórios verde.

## O que fica para o construtor

- As bibliotecas (PipeWire, D-Bus, FFT) e a organização interna (crate próprio, como o `lucernad-system`, com feature no binário).
- **A feature `spectrum` entra nas padrão do binário** e o shell a trata como obrigatória, como o módulo de sistema (decisão do construtor do documento, seguindo o "sem reserva"). A `libpipewire` (e o que o crate precisar para compilar, como o `clang` do bindgen) passa a ser dependência de compilação: o CI do `lucernad`, o `dev/lucernad.sh`/`dev/Dockerfile` do shell e o README de instalação precisam dela. O CI continua checando o build sem módulos (`--no-default-features`).
- Tamanho da FFT, sobreposição, escala das bandas e as constantes de suavização e ganho.
- Como ligar o fluxo de captura ao nó do player no PipeWire sem mudar o roteamento do usuário.
- Como a fumaça do shell exercita o anel no container.
