#!/bin/sh
set -eu

backend='@BACKEND@'
plugin='@PLUGIN@'
kwin_lib='@KWIN_LIB@'
kwin_version='@KWIN_VERSION@'

remove_link() {
    if [ "$(readlink "$plugin" || true)" = "$backend" ]; then
        rm -- "$plugin"
    fi
}

case "$1" in
    configure|triggered|abort-upgrade|abort-remove|abort-deconfigure|failed-upgrade)
        installed=$(dpkg-query -W -f='${Version}\n' kwin-wayland libkwin6 2>/dev/null || true)
        if [ "$installed" = "$kwin_version
$kwin_version" ] && [ -x /usr/bin/kwin_wayland ] && [ -f "$kwin_lib" ] && [ -f "$backend" ]; then
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
