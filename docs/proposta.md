# Proposta do lucernad

O `lucernad` é o daemon em Rust com a lógica do shell Lucerna. O shell fica só com a interface. Este documento vem da seção "Separação da lógica" das Pendências do [`lucerna-shell`](https://github.com/lucerna-desktop/lucerna-shell) e do [ecossistema](https://github.com/lucerna-desktop/lucerna-sdk/blob/main/docs/ecossistema.md).

## Princípio da migração

O daemon é opcional. Cada `services/` do shell mantém as mesmas propriedades e vira um adaptador: com o daemon rodando, lê dele; sem ele, usa a implementação atual. Um módulo por vez.

Fica no shell: as APIs nativas do Quickshell (PipeWire, MPRIS, Bluetooth, rede, notificações, PAM, Hyprland), a coordenação dos painéis, os temas, a geometria dos monitores nas configurações, a formatação e as animações.

## Módulos, em ordem de prioridade

1. **Espectro:** captura do PipeWire e FFT próprias no lugar do `cava` (hoje `services/spectrum/Cava.qml`); detecção de batida depois.
2. **Sistema:** CPU, memória, rede, discos, temperaturas e GPU no lugar do parse em `services/SystemStats.qml`, do `gpu-status.sh`, do `drm-fdinfo.sh`, do `df` e do `lsblk`. GPU pelo fdinfo e, na NVIDIA, pela NVML, sem `nvidia-smi`.
3. **Área de transferência:** observador do Wayland, histórico com deduplicação e imagens em SQLite, no lugar do `wl-paste --watch`, do base64 decodificado em JS e do `clipboard.json`.
4. **Energia e brilho:** histórico do UPower e perfis de energia por D-Bus (hoje `busctl`), brilho interno e DDC/CI por biblioteca (hoje `brightnessctl` e `ddcutil`).
5. **Rede externa:** letras (LRCLIB) e clima (Open-Meteo), com cache em disco (hoje sem cache).
6. **Launcher:** índice de apps com pontuação, frecência de verdade (hoje só contagem), busca de arquivos (hoje `fd`/`find`) e metadados de apps (hoje `app-info.sh`).
7. **Hyprland:** binds, entrada, keymap (hoje `xkbcli`) e monitores pelo socket IPC do Hyprland, no lugar do `hyprctl eval` e da geração de Lua.
8. **Luz noturna:** gama direto pelo protocolo `wlr-gamma-control`, sem o `hyprsunset`.

## Ferramenta à parte

`lucerna-video`: o conversor de papel de parede em vídeo (hoje `video-wallpaper.sh`, com `ffmpeg`), em lote, fora do daemon.

## Em aberto

- A forma: um daemon com módulos ou vários binários.
- O protocolo com o shell, que vem do `lucerna-sdk` (proposta: socket Unix com mensagens JSON).
- O empacotamento e o versionamento do protocolo entre os repositórios.
