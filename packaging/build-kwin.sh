#!/usr/bin/env bash
set -euo pipefail
umask 022

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
build_dir=$(realpath -m -- "${PICALENS_BUILD_DIR:-$PWD/build/deb}")

. /etc/os-release
case "$ID:$VERSION_CODENAME" in
    debian:trixie) target=kwin-debian13 ;;
    ubuntu:stonking) target=kwin-ubuntu26.10 ;;
    ubuntu:tenacious) target=kwin-ubuntu27.04 ;;
    *) printf 'Build the KWin backend on Debian 13, Ubuntu 26.10, or Ubuntu 27.04.\n' >&2; exit 1 ;;
esac

kwin_version=$(dpkg-query -W -f='${Version}' kwin-dev)
for package in kwin-wayland libkwin6; do
    if [[ $(dpkg-query -W -f='${Version}' "$package") != "$kwin_version" ]]; then
        printf '%s must match kwin-dev %s.\n' "$package" "$kwin_version" >&2
        exit 1
    fi
done

kwin_build=$build_dir/$target
artifact=$build_dir/kwin-backends/$target
cmake -S kwin -B "$kwin_build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DKDE_INSTALL_PLUGINDIR=/usr/lib/picalens
cmake --build "$kwin_build"
install -Dm644 "$kwin_build/picalens.so" "$artifact/picalens.so"
strip --strip-unneeded "$artifact/picalens.so"
printf '%s %s\n' "$(dpkg --print-architecture)" "$kwin_version" > "$artifact/abi"
