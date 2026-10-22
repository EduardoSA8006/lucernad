#!/usr/bin/env bash
# Roda a sonda do D-Bus (tests/sonda.rs) num D-Bus do sistema de mentira: um
# dbus-daemon próprio, num XDG_RUNTIME_DIR temporário, sem nenhum socket do
# host. A sonda registra nele um logind, um UPower e um serviço de perfis
# falsos. Só roda dentro de um container: o logind, o UPower e os perfis da
# máquina nunca são tocados.
#
# Uso (da raiz do repositório, com o toolchain do rust-toolchain.toml):
#   docker run --rm -v "$PWD:/src:ro" -v <toolchain>:/tc:ro archlinux:latest \
#     bash -c 'pacman -Syu --noconfirm --needed dbus gcc > /dev/null \
#       && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-power/tests/isolado.sh'
set -euo pipefail

if [[ ! -e /.dockerenv && ! -e /run/.containerenv ]]; then
    echo "isolado.sh: só roda dentro de um container (sobe um D-Bus do sistema de mentira)" >&2
    exit 1
fi

crate=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo=$(cd "$crate/../.." && pwd)

base=$(mktemp -d)
export XDG_RUNTIME_DIR=$base/run
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
bus=$XDG_RUNTIME_DIR/system_bus
# Nada herdado do ambiente aponta para outro barramento.
unset DBUS_SESSION_BUS_ADDRESS DBUS_SYSTEM_BUS_ADDRESS

dbus_pid=
cleanup() {
    if [[ -n $dbus_pid ]]; then
        kill -- "-$dbus_pid" 2> /dev/null || true
    fi
    rm -rf "$base"
}
trap cleanup EXIT

# A config de sessão deixa o teste ter os nomes do logind, do UPower e dos
# perfis (a do sistema só os daria a quem tem a política deles).
setsid dbus-daemon --config-file=/usr/share/dbus-1/session.conf \
    --address="unix:path=$bus" --nofork > "$base/dbus.log" 2>&1 &
dbus_pid=$!

for _ in $(seq 100); do
    [[ -S $bus ]] && break
    sleep 0.1
done
if [[ ! -S $bus ]]; then
    echo "isolado.sh: o dbus-daemon não abriu o $bus em 10 s" >&2
    cat "$base/dbus.log" >&2
    exit 1
fi

# O repositório vem montado só para leitura: a cópia é que compila.
work=$base/src
mkdir -p "$work"
tar -C "$repo" --exclude=./target --exclude=./.git -cf - . | tar -C "$work" -xf -
cd "$work"
DBUS_SYSTEM_BUS_ADDRESS=unix:path=$bus LUCERNA_ISOLADO=1 \
    cargo test -p lucernad-power --test sonda --locked -- --ignored --test-threads=1
