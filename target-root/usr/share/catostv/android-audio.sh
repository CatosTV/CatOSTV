#!/system/bin/sh
# Keep Android's media gain at unity. The host owns volume and mute.
# Read by Android's shell through the container watcher; no settings UI.

keep_media_volume_max() {
    [ "$(getprop sys.boot_completed 2>/dev/null)" = 1 ] || return 0
    state=$(timeout 5 cmd media_session volume --stream 3 --get 2>/dev/null) || return 1
    values=$(printf '%s\n' "$state" | tr -d '\r' | sed -n 's/.*volume is \([0-9][0-9]*\) in range \[[0-9][0-9]*\.\.\([0-9][0-9]*\)\].*/\1 \2/p')
    read -r level maximum <<EOF
$values
EOF
    case "$level:$maximum" in
        *[!0-9:]*|:*|*:) return 1 ;;
    esac
    [ "$maximum" -gt 0 ] && [ "$maximum" -le 1000 ] || return 1
    [ "$level" -le "$maximum" ] || return 1
    [ "$level" = "$maximum" ] && return 0
    # Android reports muted streams as zero; setting a positive level also
    # clears that internal mute. Host mute is untouched.
    timeout 5 cmd media_session volume --stream 3 --set "$maximum" >/dev/null 2>&1 || return 1
    echo "Android media volume restored to $maximum/$maximum"
}

case "${1:-}" in
    --once) keep_media_volume_max ;;
    "") while true; do keep_media_volume_max || :; sleep 1; done ;;
    *) exit 2 ;;
esac
