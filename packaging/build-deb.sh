#!/usr/bin/env bash
set -euo pipefail
umask 022

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo=$PWD
build_dir=$(realpath -m -- "${PICALENS_BUILD_DIR:-$repo/build/deb}")
mkdir -p -- "$build_dir"

architecture=$(dpkg --print-architecture)
multiarch=$(dpkg-architecture -qDEB_HOST_MULTIARCH)
app_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
deb_version=$app_version-1
dpkg --validate-version "$deb_version"

work=$(mktemp -d "$build_dir/stage.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
stage=$work/picalens
install -d "$stage/DEBIAN" "$work/debian"

for target in kwin-debian13 kwin-ubuntu26.10; do
    artifact=$build_dir/kwin-backends/$target
    if [[ ! -s $artifact/picalens.so || ! -s $artifact/abi ]]; then
        printf 'Missing KWin build artifacts in %s. Run packaging/build-kwin.sh on the target distribution.\n' "$artifact" >&2
        exit 1
    fi
    read -r backend_arch kwin_version < "$artifact/abi"
    if [[ $backend_arch != "$architecture" ]]; then
        printf 'KWin backend %s has architecture %s, expected %s.\n' "$target" "$backend_arch" "$architecture" >&2
        exit 1
    fi
    dpkg --validate-version "$kwin_version"
    backend=$stage/usr/lib/picalens/kwin/backends/$kwin_version/picalens.so
    if [[ -e $backend ]]; then
        printf 'Duplicate KWin backend for %s.\n' "$kwin_version" >&2
        exit 1
    fi
    install -Dm644 "$artifact/picalens.so" "$backend"
done

export CARGO_TARGET_DIR="$build_dir/cargo"
cargo build --locked --release --bin picalens
install -Dm755 "$CARGO_TARGET_DIR/release/picalens" "$stage/usr/bin/picalens"
strip --strip-unneeded "$stage/usr/bin/picalens"
install -Dm644 data/io.github.LoveInIdleness.PicaLens.desktop \
    "$stage/usr/share/applications/io.github.LoveInIdleness.PicaLens.desktop"
install -Dm644 data/io.github.LoveInIdleness.PicaLens.png \
    "$stage/usr/share/pixmaps/io.github.LoveInIdleness.PicaLens.png"
gnome_extension=picalens@love-in-idleness.github.io
install -Dm644 -t "$stage/usr/share/gnome-shell/extensions/$gnome_extension" \
    "gnome/$gnome_extension/metadata.json" "gnome/$gnome_extension/extension.js"
install -d "$stage/usr/share/kglobalaccel"
ln -s ../applications/io.github.LoveInIdleness.PicaLens.desktop \
    "$stage/usr/share/kglobalaccel/io.github.LoveInIdleness.PicaLens.desktop"
docs=$stage/usr/share/doc/picalens
install -Dm644 packaging/copyright "$docs/copyright"
install -Dm644 src/shaders/LICENSES "$docs/shader-licenses.txt"
host=$(rustc --print host-tuple)
cargo metadata --locked --format-version 1 --filter-platform "$host" \
    | python3 packaging/rust-notices.py | gzip -n > "$docs/rust-dependencies.txt.gz"

printf 'Source: picalens\n' > "$work/debian/control"
shlibs=$(cd "$work" && dpkg-shlibdeps -O -e"$stage/usr/bin/picalens")
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
Depends: $depends
Recommends: pipewire, wireplumber, xdg-desktop-portal, libegl1
Suggests: kwin-wayland, xdg-desktop-portal-kde, gnome-shell, gamescope
Homepage: https://github.com/Love-in-idleness/PicaLens
Description: Magnify Wayland windows and launch games with Gamescope
 PicaLens offers interactive magnification with its Plasma and GNOME Shell
 components, and Portal previews on compatible desktops. It can also launch
 games with the optional Gamescope package. Open it from the application
 launcher or run picalens toggle.
EOF

# Let KWin discover the optional plugin only while its packaged ABI matches.
kwin_lib=/usr/lib/$multiarch/libkwin.so.6
plugin=/usr/lib/$multiarch/qt6/plugins/kwin/effects/plugins/picalens.so
sed -e "s|@PLUGIN@|$plugin|" -e "s|@KWIN_LIB@|$kwin_lib|" \
    packaging/kwin-link.sh > "$stage/DEBIAN/postinst"
install -m755 "$stage/DEBIAN/postinst" "$stage/DEBIAN/prerm"
chmod 755 "$stage/DEBIAN/postinst"
printf 'interest-noawait /usr/bin/kwin_wayland\ninterest-noawait %s\n' "$kwin_lib" > "$stage/DEBIAN/triggers"

(cd "$stage" && find usr -type f -print0 | sort -z | xargs -0 md5sum) > "$stage/DEBIAN/md5sums"
deb=$build_dir/picalens_${deb_version}_${architecture}.deb
dpkg-deb --root-owner-group --build "$stage" "$deb"
(cd "$build_dir" && sha256sum "${deb##*/}" > SHA256SUMS)
printf '%s\n' "$deb"
