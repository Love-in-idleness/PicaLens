#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
read -r version architecture < mutter-version
if [[ ! $version =~ ^51\.[0-9A-Za-z.+~-]+$ ||
    $(dpkg --print-architecture) != "$architecture" ||
    $(dpkg-query -W -f='${db:Status-Abbrev} ${Version}' "libmutter-51-0:$architecture") != "ii  $version" ]]; then
    printf 'This component needs installed libmutter-51-0 %s (%s).\n' "$version" "$architecture" >&2
    exit 1
fi

mapfile -t shells < <(systemctl --user list-units --type=service --state=running --plain \
    --no-legend 'org.gnome.Shell@*.service' | awk '{print $1}')
if [[ ${#shells[@]} != 1 || ! ${shells[0]} =~ ^org\.gnome\.Shell@[a-zA-Z0-9_.-]+\.service$ ]]; then
    printf 'Run this from a GNOME Wayland session managed by systemd.\n' >&2
    exit 1
fi
unit=${shells[0]}
dropin="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$unit.d/50-picalens-mutter.conf"
exec_start=$(systemctl --user show -p ExecStart --value "$unit")
if [[ $exec_start != *'path=/usr/bin/gnome-shell ;'* &&
    $exec_start != *'/.local/share/picalens/mutter/'*'/run-gnome-shell ;'* ]]; then
    printf '%s has a custom ExecStart. Review it before enabling PicaLens.\n' "$unit" >&2
    exit 1
fi

relative=".local/share/picalens/mutter/$version"
install -Dm755 run-gnome-shell "$HOME/$relative/run-gnome-shell"
install -Dm644 libmutter-51.so.0 "$HOME/$relative/libmutter-51.so.0"
install -d -- "$(dirname -- "$dropin")"
printf '[Service]\nExecStart=\nExecStart="%%h/%s/run-gnome-shell" --mode=%%i\n' "$relative" > "$dropin"
systemctl --user daemon-reload
printf 'Configured %s. Sign out and back in to load the component.\n' "$dropin"
