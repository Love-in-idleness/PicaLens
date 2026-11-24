#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
    printf 'Usage: %s /path/to/mutter_51.x.dsc\n' "${0##*/}" >&2
    exit 1
fi
source_package=$(realpath -e -- "$1")
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo=$PWD
component=$repo/gnome/mutter
output=$(realpath -m -- "${PICALENS_BUILD_DIR:-$repo/build/deb}")
mkdir -p -- "$output"
work=$(mktemp -d "$output/mutter-build.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

version=$(dpkg-query -W -f='${Version}' libmutter-51-0)
architecture=$(dpkg --print-architecture)
multiarch=$(dpkg-architecture -qDEB_HOST_MULTIARCH)
dpkg-source -x "$source_package" "$work/source"
source_control=$work/source/debian/changelog
if [[ $(dpkg-parsechangelog -l "$source_control" -S Source) != mutter ||
    $(dpkg-parsechangelog -l "$source_control" -S Version) != "$version" ||
    ! $version =~ ^51\.[0-9A-Za-z.+~-]+$ ]]; then
    printf 'Use the exact GNOME 51 Mutter source package matching installed libmutter-51-0.\n' >&2
    exit 1
fi
patch --directory "$work/source" -p1 < "$component/51-xwayland-input-transform.patch"
meson setup "$work/build" "$work/source" --prefix=/usr --libdir="lib/$multiarch" \
    --buildtype=release -Dremote_desktop=true -Dintrospection=false \
    -Dtests=disabled -Dinstalled_tests=false -Ddocs=false
ninja -C "$work/build" src/libmutter-51.so.0.0.0

name=picalens-gnome-xwayland_${version}_$architecture
stage=$work/$name
install -Dm644 "$work/build/src/libmutter-51.so.0.0.0" "$stage/libmutter-51.so.0"
strip --strip-unneeded "$stage/libmutter-51.so.0"
patchelf --force-rpath --set-rpath "/usr/lib/$multiarch/mutter-51" "$stage/libmutter-51.so.0"
ldd -r "$stage/libmutter-51.so.0" > "$work/ldd"
if grep -E 'not found|undefined symbol' "$work/ldd"; then
    exit 1
fi
install -m755 -t "$stage" "$component/run-gnome-shell" "$component/install-user.sh"
install -m644 -t "$stage" "$work/source/COPYING" "$component/51-xwayland-input-transform.patch"
printf '%s %s\n' "$version" "$architecture" > "$stage/mutter-version"
tar -czf "$output/$name.tar.gz" -C "$work" "$name"
printf '%s\n' "$output/$name.tar.gz"
