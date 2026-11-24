#!/usr/bin/env bash
set -euo pipefail
umask 022

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo=$PWD
build_dir=$(realpath -m -- "${PICALENS_BUILD_DIR:-$repo/build/deb}")
mkdir -p -- "$build_dir"

. /etc/os-release
case "$ID:$VERSION_CODENAME" in
    debian:trixie) target=debian13 ;;
    ubuntu:stonking) target=ubuntu26.10 ;;
    ubuntu:tenacious) target=ubuntu27.04 ;;
    *) printf 'Build on Debian 13, Ubuntu 26.10, or Ubuntu 27.04.\n' >&2; exit 1 ;;
esac

kwin_version=$(dpkg-query -W -f='${Version}' kwin-dev)
for package in kwin-wayland libkwin6; do
    if [[ $(dpkg-query -W -f='${Version}' "$package") != "$kwin_version" ]]; then
        printf '%s must match kwin-dev %s.\n' "$package" "$kwin_version" >&2
        exit 1
    fi
done

qt_plugins=$(qtpaths6 --plugin-dir)
kwin_lib=$(dpkg-query -L libkwin6 | sed -n '/\/libkwin\.so\.6$/p')
test -f "$kwin_lib"
backend_root=/usr/lib/picalens
backend_path=$backend_root/kwin/effects/plugins/picalens.so
architecture=$(dpkg --print-architecture)
app_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
deb_version=$app_version-1$target
dpkg --validate-version "$deb_version"

export CARGO_TARGET_DIR="$build_dir/cargo"
cargo build --locked --release --bin picalens
cmake -S kwin -B "$build_dir/kwin" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DKDE_INSTALL_PLUGINDIR="$backend_root"
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
sed -e "s|@BACKEND@|$backend_path|" \
    -e "s|@PLUGIN@|$qt_plugins/kwin/effects/plugins/picalens.so|" \
    -e "s|@KWIN_LIB@|$kwin_lib|" -e "s|@KWIN_VERSION@|$kwin_version|" \
    packaging/kwin-link.sh > "$stage/DEBIAN/postinst"
install -m755 "$stage/DEBIAN/postinst" "$stage/DEBIAN/prerm"
chmod 755 "$stage/DEBIAN/postinst"
printf 'interest-noawait /usr/bin/kwin_wayland\ninterest-noawait %s\n' "$kwin_lib" > "$stage/DEBIAN/triggers"

(cd "$stage" && find usr -type f -print0 | sort -z | xargs -0 md5sum) > "$stage/DEBIAN/md5sums"
deb=$build_dir/picalens_${deb_version}_${architecture}.deb
dpkg-deb --root-owner-group --build "$stage" "$deb"
(cd "$build_dir" && sha256sum "${deb##*/}" > SHA256SUMS)
printf '%s\n' "$deb"
