#!/usr/bin/env bash
# Roda a sonda do backend Wayland (tests/sonda.rs) num compositor isolado: um
# sway headless próprio, num XDG_RUNTIME_DIR temporário, sem nenhum socket do
# host. Só roda dentro de um container: a sessão do usuário nunca é tocada.
#
# Uso (da raiz do repositório, com o toolchain do rust-toolchain.toml; o
# `setcap -r` porque o sway do Arch tem capabilities de arquivo que o
# container recusa):
#   docker run --rm -v "$PWD:/src:ro" -v <toolchain>:/tc:ro archlinux:latest \
#     bash -c 'pacman -Syu --noconfirm --needed sway wl-clipboard libcap gcc > /dev/null && setcap -r /usr/bin/sway \
#       && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-clipboard/tests/isolado.sh'
set -euo pipefail

if [[ ! -e /.dockerenv && ! -e /run/.containerenv ]]; then
    echo "isolado.sh: só roda dentro de um container (sobe um compositor de teste)" >&2
    exit 1
fi

crate=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo=$(cd "$crate/../.." && pwd)

base=$(mktemp -d)
export XDG_RUNTIME_DIR=$base/run
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
# Nada herdado do ambiente aponta para outra sessão.
unset WAYLAND_DISPLAY DISPLAY WAYLAND_SOCKET

sway_pid=
cleanup() {
    if [[ -n $sway_pid ]]; then
        kill -- "-$sway_pid" 2> /dev/null || true
    fi
    rm -rf "$base"
}
trap cleanup EXIT

WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    setsid sway -c /dev/null > "$base/sway.log" 2>&1 &
sway_pid=$!

for _ in $(seq 100); do
    if [[ -S $XDG_RUNTIME_DIR/wayland-1 ]]; then
        break
    fi
    sleep 0.1
done
if [[ ! -S $XDG_RUNTIME_DIR/wayland-1 ]]; then
    echo "isolado.sh: o socket wayland-1 do sway não apareceu em 10 s" >&2
    ls -la "$XDG_RUNTIME_DIR" >&2
    tail -n 20 "$base/sway.log" >&2
    exit 1
fi

# O repositório vem montado só para leitura: a cópia é que compila.
work=$base/src
mkdir -p "$work"
tar -C "$repo" --exclude=./target --exclude=./.git -cf - . | tar -C "$work" -xf -
cd "$work"
cargo build --locked -p lucernad-clipboard --example selecao
export LUCERNA_SELECAO=$work/target/debug/examples/selecao

sonda() {
    LUCERNA_ISOLADO=1 LUCERNA_SWAY_PID=$sway_pid \
        cargo test -p lucernad-clipboard --test sonda --locked -- --ignored "$@"
}
sonda --test-threads=1 --skip compositor_drop
# Por último e sozinho: este mata o sway.
sonda --exact compositor_drop_is_an_event
