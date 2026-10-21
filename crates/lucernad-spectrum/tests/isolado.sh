#!/usr/bin/env bash
# Roda a sonda do backend do PipeWire (tests/sonda.rs) num PipeWire isolado:
# D-Bus, PipeWire e WirePlumber próprios, num XDG_RUNTIME_DIR temporário, com
# o sink nulo e o microfone falso de testdata/pipewire/ e sem os monitores de
# hardware. Só roda dentro de um container: o PipeWire de teste toca som.
#
# Uso (da raiz do repositório, com o toolchain do rust-toolchain.toml):
#   docker run --rm -v "$PWD:/src:ro" -v <toolchain>:/tc:ro archlinux:latest \
#     bash -c 'pacman -Syu --noconfirm --needed pipewire wireplumber libpipewire clang pkgconf gcc dbus > /dev/null \
#       && useradd -m t && runuser -u t -- env PATH=/tc/bin:$PATH bash /src/crates/lucernad-spectrum/tests/isolado.sh'
set -euo pipefail

if [[ ! -e /.dockerenv && ! -e /run/.containerenv ]]; then
    echo "isolado.sh: só roda dentro de um container (o PipeWire de teste toca som)" >&2
    exit 1
fi

crate=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo=$(cd "$crate/../.." && pwd)

base=$(mktemp -d)
export XDG_RUNTIME_DIR=$base/run XDG_CONFIG_HOME=$base/config
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d" \
    "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d"
chmod 700 "$XDG_RUNTIME_DIR"
cp "$crate/testdata/pipewire/pipewire.conf" "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/"
cp "$crate/testdata/pipewire/wireplumber.conf" "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/"
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus

pids=()
cleanup() {
    for pid in "${pids[@]}"; do
        kill -- "-$pid" 2> /dev/null || true
    done
    rm -rf "$base"
}
trap cleanup EXIT

setsid dbus-daemon --session --nofork --address="$DBUS_SESSION_BUS_ADDRESS" > "$base/dbus.log" 2>&1 &
pids+=($!)
setsid pipewire > "$base/pipewire.log" 2>&1 &
pids+=($!)
setsid wireplumber > "$base/wireplumber.log" 2>&1 &
pids+=($!)

for _ in $(seq 100); do
    if wpctl status 2> /dev/null | grep -q lucerna-nulo; then
        break
    fi
    sleep 0.1
done
if ! wpctl status 2> /dev/null | grep -q lucerna-nulo; then
    echo "isolado.sh: o sink lucerna-nulo não apareceu em 10 s" >&2
    tail -n 20 "$base"/*.log >&2
    exit 1
fi

# O repositório vem montado só para leitura: a cópia é que compila.
work=$base/src
mkdir -p "$work"
tar -C "$repo" --exclude=./target --exclude=./.git -cf - . | tar -C "$work" -xf -
cd "$work"
cargo test -p lucernad-spectrum --test sonda --locked -- --ignored --test-threads=1
