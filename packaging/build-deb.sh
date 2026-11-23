#!/usr/bin/env bash
set -euo pipefail
umask 022

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo=$PWD
build_dir=$(realpath -m -- "${PICALENS_BUILD_DIR:-$repo/build/deb}")
mkdir -p -- "$build_dir"

. /etc/os-release
if [[ $ID != debian || $VERSION_CODENAME != trixie ]]; then
    printf 'Build this package on Debian 13 (trixie).\n' >&2
    exit 1
fi

kwin_version=$(dpkg-query -W -f='${Version}' kwin-dev)
for package in kwin-wayland libkwin6; do
    if [[ $(dpkg-query -W -f='${Version}' "$package") != "$kwin_version" ]]; then
        printf '%s must match kwin-dev %s.\n' "$package" "$kwin_version" >&2
        exit 1
    fi
done

qt_plugins=$(qtpaths6 --plugin-dir)
architecture=$(dpkg --print-architecture)
app_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
deb_version=$app_version-1
dpkg --validate-version "$deb_version"

export CARGO_TARGET_DIR="$build_dir/cargo"
cargo build --locked --release --bin picalens
cmake -S kwin -B "$build_dir/kwin" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DKDE_INSTALL_PLUGINDIR="$qt_plugins"
cmake --build "$build_dir/kwin"

work=$(mktemp -d "$build_dir/stage.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
stage=$work/picalens
install -d "$stage/DEBIAN" "$work/debian"
DESTDIR="$stage" cmake --install "$build_dir/kwin" --strip
install -Dm755 "$CARGO_TARGET_DIR/release/picalens" "$stage/usr/bin/picalens"
strip --strip-unneeded "$stage/usr/bin/picalens"
install -Dm644 data/io.github.LoveInIdleness.PicaLens.desktop \
    "$stage/usr/share/applications/io.github.LoveInIdleness.PicaLens.desktop"
docs=$stage/usr/share/doc/picalens
install -Dm644 packaging/copyright "$docs/copyright"
host=$(rustc --print host-tuple)
cargo metadata --locked --format-version 1 --filter-platform "$host" \
    | python3 packaging/rust-notices.py | gzip -n > "$docs/rust-dependencies.txt.gz"

printf 'Source: picalens\n' > "$work/debian/control"
shlibs=$(cd "$work" && dpkg-shlibdeps -O -xlibkwin6 \
    -e"$stage/usr/bin/picalens" -e"$stage$qt_plugins/kwin/effects/plugins/picalens.so")
depends=${shlibs#shlibs:Depends=}
if [[ -z $depends || $depends == "$shlibs" ]]; then
    printf 'Could not determine shared library dependencies.\n' >&2
    exit 1
fi

cat > "$stage/DEBIAN/control" <<EOF
Package: picalens
Version: $deb_version
Architecture: $architecture
Section: utils
Priority: optional
Maintainer: PicaLens <93856868+Love-in-idleness@users.noreply.github.com>
Installed-Size: $(du -sk "$stage/usr" | cut -f1)
Depends: kwin-wayland (= $kwin_version), libkwin6 (= $kwin_version), $depends
Recommends: pipewire, wireplumber, xdg-desktop-portal-kde
Homepage: https://github.com/Love-in-idleness/PicaLens
Description: Magnify an open window on Plasma Wayland
 PicaLens interactively magnifies existing native Wayland and XWayland windows
 on Plasma. Open it from the application launcher or run picalens toggle.
EOF

(cd "$stage" && find usr -type f -print0 | sort -z | xargs -0 md5sum) > "$stage/DEBIAN/md5sums"
deb=$build_dir/picalens_${deb_version}_${architecture}.deb
dpkg-deb --root-owner-group --build "$stage" "$deb"
(cd "$build_dir" && sha256sum "${deb##*/}" > SHA256SUMS)
printf '%s\n' "$deb"
