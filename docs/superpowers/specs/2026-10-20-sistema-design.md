# lucernad: o daemon e o módulo de sistema

**Data:** 2026-10-20
**Base:** `lucerna-sdk/docs/superpowers/specs/2026-10-19-protocolo-design.md` (o protocolo, o serviço sob demanda, o manifesto e a linha de comando, já implementados no `lucerna-sdk`) e `lucerna-sdk/docs/ecossistema.md`.
**Repositórios:** este trabalho mexe em três — `lucerna-sdk` (uma mudança pequena), `lucernad` (o daemon e o módulo) e `lucerna-shell` (o cliente do protocolo e o adaptador). Este documento fica no `lucernad` e vale para os três.

## Por quê

O shell Lucerna é só a interface; a lógica vai para código próprio em Rust. Hoje o `services/SystemStats.qml` do shell mede o sistema lendo `/proc` e `/sys` em QML e rodando ferramentas externas a cada 2 s com a aba aberta: `df`, `lsblk`, `nvidia-smi`, `qs --version`, `sh` com `cat`/`grep`/`awk`/`sed`, e os scripts `services/scripts/gpu-status.sh` e `services/scripts/drm-fdinfo.sh` (este faz `grep` em todos os `/proc/*/fdinfo/*`). São de 4 a 6 processos por ciclo. O módulo `system` do `lucernad` faz o mesmo com código próprio, num processo só, e estreia o caminho shell ↔ daemon que os outros módulos (espectro, energia, Hyprland, luz noturna, área de transferência, brilho…) vão usar.

## Decisões do usuário

- **Um binário só, `lucernad`**, com um módulo por área; este é o primeiro (`system`).
- **Sem reserva:** o `lucernad` vira dependência do shell neste projeto. O código antigo de medição e as ferramentas externas saem do shell neste mesmo trabalho; sem o daemon, a aba Desempenho e o card de recursos da aba Painel mostram "indisponível".
- O módulo cobre tudo o que o shell mede hoje (abaixo). Nada novo na interface neste projeto (o medidor de bateria na aba Desempenho ficou para depois).

## Objetivos

- Medir CPU, temperatura, memória, swap, rede, tempo ligado, os dados da máquina, os discos e as GPUs sem nenhum processo externo.
- O shell continua idêntico para quem usa: as mesmas propriedades no `SystemStats.qml`, os mesmos valores, os mesmos cards.
- Medir só com alguém olhando (a aba aberta), como hoje.
- Não acordar a GPU dedicada para ler o uso dela.
- Deixar pronto, no shell, um cliente do protocolo genérico que os próximos módulos reaproveitam.

## Não objetivos

- Informação nova na interface (bateria, processos, ventoinhas, consumo da CPU em W).
- Os outros módulos do `lucernad`.
- Publicar os crates ou empacotar no AUR.

## Anti-objetivos

- Trocar uma ferramenta externa por outra (nada de chamar `lsblk` ou `nvidia-smi` de dentro do Rust).
- Uma interface diferente da de hoje.
- O daemon trabalhando com a aba fechada.

## O `lucernad`

- **Um binário**, `lucernad`, escrito sobre o `lucerna-sdk` (`lucerna-service` para o serviço sob demanda, `lucerna-ipc` para os tópicos, `lucerna-cli` para a linha de comando, `lucerna-paths`, `lucerna-manifest`), seguindo o molde do `lucerna-hello`: sem argumentos mostra a ajuda; `lucernad serve` é o serviço; os subcomandos comuns `status` e `watch <tópico>` servem para depurar.
- **Id do app:** `lucernad`; socket `$XDG_RUNTIME_DIR/lucerna/lucernad.sock`; unidades `lucerna-lucernad.socket` e `lucerna-lucernad.service` do systemd do usuário (o nome segue a regra do SDK, `lucerna-<app>`). Saída por ociosidade com o padrão do SDK (60 s).
- **Manifesto** `lucernad.json` sem painel (o `lucernad` é só serviço).
- **Módulos como features do Cargo** (`system` ligado por padrão). Cada módulo registra os seus tópicos e métodos no mesmo servidor, com o nome do módulo na frente (`system.stats`). Um módulo que não consegue iniciar fica indisponível sem derrubar os outros (anunciado num tópico de estado do módulo, ver abaixo).
- **Organização do código:** um crate de workspace por módulo quando fizer sentido (`lucernad-system`) ou módulos dentro do crate do binário — escolha do construtor, desde que a lógica de leitura e de conta fique em funções puras testáveis com arquivos de exemplo.

## O módulo `system`

### O que mede (o mesmo que o `SystemStats.qml` de hoje)

| Grupo | Dados | De onde (código próprio) |
| --- | --- | --- |
| Máquina (fixo) | modelo da CPU, threads, hostname, nome do SO, kernel | `/proc/cpuinfo`, `/proc/sys/kernel/hostname`, `/etc/os-release`, `/proc/sys/kernel/osrelease` |
| CPU | uso (0–1) entre duas leituras | `/proc/stat` |
| Temperatura | a do pacote da CPU (°C), escolhendo o sensor como hoje (`coretemp`/`k10temp`…) | `/sys/class/hwmon/*/name` e `temp*_input` |
| Memória e swap | total e usado | `/proc/meminfo` (a mesma conta de "usado" de hoje) |
| Rede | taxa de download e upload, totais, e o histórico das taxas (30 pontos, para o gráfico) | `/proc/net/dev`, só interfaces físicas (a regra de hoje) |
| Tempo ligado | segundos | `/proc/uptime` |
| Discos | por disco físico: nome (`nvme0n1`), tamanho e usado (soma das partições montadas), se é o da raiz; o da raiz primeiro | `/proc/self/mountinfo` + `statvfs` + a árvore de `/sys/class/block` (pai da partição, `slaves/` para LUKS/LVM, `dm/name`) |
| GPUs | por placa: fabricante, nome, estado (ativa/suspensa), uso (0–1 ou ausente), temperatura, frequência e frequência máxima | `/sys/class/drm/card*`; AMD `gpu_busy_percent`; Intel pelo fdinfo do DRM (`drm-engine-*`/`drm-cycles-*`, a conta de `drmBusy` de hoje), lendo só os fds que apontam para `/dev/dri/*`; NVIDIA pela NVML (`libnvidia-ml.so`, carregada em tempo de execução). **Nomes iguais aos de hoje** (decisão do construtor do documento, pela regra "os mesmos valores"): NVIDIA pelo `Model:` de `/proc/driver/nvidia/gpus/<pci>/information` (funciona com a placa suspensa); AMD e Intel com os nomes genéricos de hoje ("AMD Radeon", "Intel (integrada)"). Sem `pci.ids`. |

O `qs --version` do `quickshellVersion` **não** vai para o daemon: é do shell, e o shell o descobre sozinho (fora deste módulo).

### Regras de comportamento

- **Só com assinante.** As amostras rodam **enquanto houver alguém assinando** `system.stats`, `system.disks` ou `system.gpus` (cada grupo só é lido se o tópico dele tem assinante); sem assinantes, nada roda. `system.info` é fixo (lido uma vez). O `lucerna-ipc` já oferece `subscribers()`, `wait_subscribed()` e `wait_unsubscribed()` para qualquer tópico, comum ou contínuo.
- **Intervalo** (decisão do construtor do documento, para manter a opção "Atualização" das Configurações, 1/2/5 s): o método `system.configure {interval_ms}` (500 a 10000; padrão 2000) muda o intervalo de amostragem; vale o último pedido. O histórico continua com 30 pontos (cobre de 30 s a 150 s conforme o intervalo, como hoje).
- **GPU dedicada dormindo não é acordada.** A NVML só é usada com o `runtime_status` da placa em `active`, e o handle é fechado entre as leituras (ou mantido aberto só se ficar provado que não impede a placa de dormir — escolha do construtor, com o critério testado na máquina real). Placa suspensa aparece como suspensa, sem uso.
- **Discos:** sistemas de arquivos de rede e virtuais ficam de fora (a regra do `-l` de hoje, pelo tipo em `mountinfo`); um `statvfs` que não responde (montagem morta) não trava o daemon (prazo por leitura). O resultado bate com o de hoje: mesmos discos, mesma soma, mesma ordem.
- **Histórico de rede** fica no daemon (30 pontos), para o gráfico já abrir cheio quando a aba abre de novo enquanto o daemon estiver de pé.

### Tópicos

Formas em JSON; os nomes dos campos seguem os do `SystemStats.qml` para o adaptador ser direto.

- `system.info` — `{cpuModel, cpuThreads, hostname, osName, kernel}`.
- `system.stats` — `{cpuUsage, cpuTemp, memTotal, memUsed, swapTotal, swapUsed, rxRate, txRate, rxTotal, txTotal, rxHistory, txHistory, uptime}` (`cpuTemp` `null` sem sensor).
- `system.disks` — `{disks: [{name, size, used, root}]}`.
- `system.gpus` — `{gpus: [{card, vendor, pci, state, name, usage, temp, freq, maxFreq}]}` (o formato de hoje; campos sem leitura vêm `null`).
- `system.status` — `{available: bool, error: string?}` do módulo (para o shell distinguir "módulo com problema" de "daemon fora").

Métodos: só `system.configure {interval_ms}` → `{}` (acima). Ligar e desligar a GPU como hoje faz o `gpuEnabled` do shell vira simplesmente não assinar `system.gpus`.

## `lucerna-sdk`

O `lucerna-ipc` já tem `subscribers()`, `wait_subscribed()` e `wait_unsubscribed()` para qualquer tópico (o builder check conferiu no código). A tarefa do SDK é só garantir isso com testes e documentação para os tópicos comuns, e corrigir o que faltar se o módulo precisar. Os crates ainda não estão publicados: o `lucernad` depende do `lucerna-sdk` **por git, numa revisão fixa** (decisão do construtor do documento), e a mesma fixação vale para o CI e o container do shell.

## Lado do shell (`lucerna-shell`)

- **Conexão permanente** (decisão do construtor do documento): o shell fica conectado ao `lucernad` enquanto roda, porque os próximos módulos (área de transferência, luz noturna) precisam disso e porque `system.info` alimenta a página Sobre das Configurações e o cabeçalho do Painel (hostname, tempo ligado) fora da aba Desempenho. Com isso o daemon só sai por ociosidade quando o shell não está rodando. As assinaturas de `system.stats`/`disks`/`gpus` seguem o `active` e o `gpuEnabled`; `system.info` fica assinado sempre. Sem o daemon, a página Sobre e o cabeçalho mostram "—" nos dados da máquina.
- **Cliente do protocolo em QML, genérico** (um só para todos os módulos do `lucernad`, e depois para os apps): conecta ao socket com o `Socket` do `Quickshell.Io`, faz o aperto de mão (`lucerna.hello`, versão `1.0`, `client` = `"shell"`, `app` = `"lucernad"`), assina tópicos, recebe os `lucerna.state` (descartando `seq` velho), chama métodos com resposta, e reconecta com espera crescente quando o daemon cai ou ainda não subiu (a ativação por socket do systemd sobe o daemon na conexão). Expõe `connected` e o último estado por tópico. Fica no `core/` ou em `services/`, conforme as camadas da casa (`ci/rules.py`), e tem testes de unidade no harness do shell (as funções puras: montar mensagens, ler linhas, descartar `seq`).
- **`services/SystemStats.qml` vira adaptador:** as mesmas propriedades públicas de hoje (`cpuModel`, `cpuThreads`, `cpuUsage`, `cpuTemp`, `memTotal`, `memUsed`, `swapTotal`, `swapUsed`, `disks`, `rxRate`, `txRate`, `rxTotal`, `txTotal`, `rxHistory`, `txHistory`, `gpus`, `uptime`, `hostname`, `osName`, `kernel`, `quickshellVersion`, `historyLength`, `active`, `gpuEnabled`, `interval` — este manda `system.configure`), agora alimentadas pelos tópicos. `active` liga e desliga as assinaturas (`stats`, `disks`); `gpuEnabled` liga e desliga a de `system.gpus`. Ganha `available` (daemon conectado e módulo disponível).
- **Sai do shell:** o parse de `/proc` e `/sys` do `SystemStats.qml` (e as funções puras correspondentes, com os testes delas, que passam a existir em Rust), o `df`, o `lsblk`, o `nvidia-smi`, os scripts `services/scripts/gpu-status.sh` e `services/scripts/drm-fdinfo.sh`, e as entradas deles em `tests/coverage.json`.
- **"Indisponível":** sem o daemon (ou com `system.status.available` falso), a aba Desempenho e o card de recursos da aba Painel mostram um estado vazio discreto com "indisponível", no lugar dos cards de medida.
- **Ambiente de teste e CI do shell:** o container de teste (`dev/test.sh`, `dev/Dockerfile`) e o CI do shell (`ci/`) passam a ter o `lucernad` instalado e rodando, para a aba Desempenho continuar sendo exercitada. Como (compilar do repositório `lucernad` numa versão fixada, ou usar um binário pronto) fica para o construtor, desde que a versão usada seja fixa e reproduzível.
- **Documentação:** README do shell (dependência nova: `lucernad`), Proposta e Pendências.

## Testes

- **Rust (`lucernad`):** cada leitura com arquivos de exemplo (`/proc/stat`, `meminfo`, `net/dev`, `cpuinfo`, `mountinfo` com bind mounts e subvolumes btrfs, árvores de `/sys/class/block` com LUKS e LVM, hwmon, `/sys/class/drm` com Intel/AMD/NVIDIA suspensa, fdinfo i915 e xe); a conta do uso da CPU, da rede e do fdinfo; o "só com assinante" (nada roda sem assinante, começa ao assinar, para ao sair); a GPU suspensa não acordada (NVML falsa nos testes); `statvfs` lento não trava. Os testes de hoje do `SystemStats.qml` (em `tests/`) servem de referência dos casos.
- **Comparação na máquina real:** os valores do `lucernad status` e da aba Desempenho batem com os de antes da migração (mesmos discos e somas, mesmas GPUs, CPU e memória coerentes).
- **SDK:** o aviso de assinantes nos tópicos comuns.
- **Shell:** o cliente do protocolo (funções puras no harness), a fumaça com o `lucernad` de pé (aba Desempenho e card de recursos sem avisos) e com ele fora ("indisponível" aparece, sem avisos).
- CI dos três repositórios verde.

## O que fica para o construtor

- A organização interna (um crate por módulo ou módulos no binário), as bibliotecas (`procfs` ou leitura própria, `rustix`/`nix` para `statvfs`, `nvml-wrapper`), desde que em código próprio e sem processos externos.
- O formato exato do aviso de assinantes no SDK.
- Como o shell instala o `lucernad` no container de teste e no CI.
- Onde o cliente QML fica nas camadas do shell.
