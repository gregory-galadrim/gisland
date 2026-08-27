#!/usr/bin/env bash
set -euo pipefail

: "${HOME:?HOME is not set}"
for command in direnv install patchelf systemctl; do command -v "$command" >/dev/null || { echo "install-local: missing $command" >&2; exit 1; }; done

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
prefix="$HOME/.local"
cd "$root"
direnv exec "$root" cargo build --release --locked

systemctl --user stop gisland.service 2>/dev/null || true
install -Dm755 target/release/gisland "$prefix/bin/gisland"
patchelf --add-rpath "${LD_LIBRARY_PATH%%:*}:/run/opengl-driver/lib" "$prefix/bin/gisland"
ln -sfn gisland "$prefix/bin/gislandctl"
rm -f "$prefix/bin/gisland-lua-host"
rm -rf "$prefix/share/gisland"
install -d "$prefix/share/gisland/modules" "$prefix/share/gisland/fonts"
install -m644 assets/*.lua "$prefix/share/gisland/"
for module in assets/modules/*; do
  name=${module##*/}
  install -d "$prefix/share/gisland/modules/$name"
  find "$module" -maxdepth 1 -type f -name '*.lua' -exec install -m644 '{}' "$prefix/share/gisland/modules/$name/" \;
done
install -m644 assets/fonts/*.ttf assets/fonts/*.otf assets/fonts/LICENSE-* "$prefix/share/gisland/fonts/"
install -Dm644 packaging/gisland.service "$HOME/.config/systemd/user/gisland.service"

rm -rf "$prefix/share/gisland/distributed" "$prefix/share/gisland/audio" "$prefix/share/gisland/battery" "$prefix/share/gisland/notifications"
systemctl --user daemon-reload
systemctl --user unset-environment LIBGL_DRIVERS_PATH LD_LIBRARY_PATH 2>/dev/null || true
loader_file=$(direnv exec "$root" printenv GDK_PIXBUF_MODULE_FILE 2>/dev/null || true)
[[ -z $loader_file ]] || export GDK_PIXBUF_MODULE_FILE=$loader_file
names=()
for name in DISPLAY XAUTHORITY PATH GDK_PIXBUF_MODULE_FILE XDG_DATA_DIRS XDG_CURRENT_DESKTOP DESKTOP_SESSION; do [[ -v $name ]] && names+=("$name"); done
((${#names[@]} == 0)) || systemctl --user import-environment "${names[@]}"
systemctl --user reset-failed gisland.service 2>/dev/null || true
systemctl --user enable gisland.service
systemctl --user restart gisland.service
for _ in {1..240}; do "$prefix/bin/gislandctl" status >/dev/null 2>&1 && { echo "gisland installed under $prefix"; exit 0; }; sleep .25; done
echo "install-local: gisland.service did not become healthy" >&2
exit 1
