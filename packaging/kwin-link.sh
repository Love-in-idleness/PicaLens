#!/bin/sh
set -eu

backend_root=/usr/lib/picalens/kwin
plugin='@PLUGIN@'
kwin_lib='@KWIN_LIB@'

remove_link() {
    case "$(readlink "$plugin" || true)" in
        "$backend_root"/backends/*/picalens.so|"$backend_root"/effects/plugins/picalens.so)
            rm -- "$plugin"
            ;;
    esac
}

case "$1" in
    configure|triggered|abort-upgrade|abort-remove|abort-deconfigure|failed-upgrade)
        installed=$(dpkg-query -W -f='${Version}\n' kwin-wayland libkwin6 2>/dev/null || true)
        kwin_version=${installed%%'
'*}
        backend=$backend_root/backends/$kwin_version/picalens.so
        if [ -n "$kwin_version" ] && [ "$installed" = "$kwin_version
$kwin_version" ] && [ -x /usr/bin/kwin_wayland ] && [ -f "$kwin_lib" ] && [ -f "$backend" ]; then
            if [ "$(readlink "$plugin" || true)" = "$backend" ]; then
                exit 0
            fi
            remove_link
            if [ ! -e "$plugin" ] && [ ! -L "$plugin" ]; then
                install -d -m755 "$(dirname "$plugin")"
                ln -s "$backend" "$plugin"
            fi
        else
            remove_link
        fi
        ;;
    remove|deconfigure|upgrade) remove_link ;;
esac
