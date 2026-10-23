#!/bin/sh
# Certbot deploy hook. Keep all key material on this host; restart only DeskLink.
set -eu
export LC_ALL=C
umask 077

lineage=/etc/letsencrypt/live/p2p.yxswy.com
if [ "${RENEWED_LINEAGE:-$lineage}" != "$lineage" ]; then
    exit 0
fi

tls=/opt/desklink-relay/tls
install -d -m 0750 -o 0 -g 65534 "$tls" "$tls/versions"
exec 9>"$tls/.renew.lock"
flock -x 9
if [ -e "$tls/current" ] && [ ! -L "$tls/current" ]; then
    echo 'DeskLink TLS current path must be a symlink; existing files are unchanged.' >&2
    exit 1
fi
previous=$(readlink "$tls/current" 2>/dev/null || true)
staged=$(mktemp -d "$tls/versions/version.XXXXXXXX")
install -m 0440 -o 65534 -g 65534 "$lineage/fullchain.pem" "$staged/fullchain.pem"
install -m 0400 -o 65534 -g 65534 "$lineage/privkey.pem" "$staged/privkey.pem"

# OpenSSL 3.0 may return success for a hostname mismatch. Check the positive text.
hostname_check=$(openssl x509 -in "$staged/fullchain.pem" -noout -checkhost turn.p2p.yxswy.com)
if [ "$hostname_check" != 'Hostname turn.p2p.yxswy.com does match certificate' ]; then
    echo 'DeskLink certificate hostname does not match; active files are unchanged.' >&2
    exit 1
fi
openssl x509 -in "$staged/fullchain.pem" -noout -checkend 86400 >/dev/null
# Separate commands preserve failures in /bin/sh without relying on pipefail.
openssl x509 -in "$staged/fullchain.pem" -pubkey -noout -out "$staged/certificate-public.pem"
openssl pkey -pubin -in "$staged/certificate-public.pem" -outform DER -out "$staged/certificate-public.der"
openssl pkey -in "$staged/privkey.pem" -pubout -outform DER -out "$staged/key-public.der"
if ! cmp -s "$staged/certificate-public.der" "$staged/key-public.der"; then
    echo 'DeskLink certificate and key do not match; active files are unchanged.' >&2
    exit 1
fi
rm -f "$staged/certificate-public.pem" "$staged/certificate-public.der" "$staged/key-public.der"
chown 0:65534 "$staged"
chmod 0750 "$staged"

set_current() {
    ln -s "$1" "$tls/current.new.$$"
    mv -Tf "$tls/current.new.$$" "$tls/current"
}
set_current "versions/${staged##*/}"
if [ "$(docker inspect --format '{{.State.Running}}' desklink-relay-relay-1 2>/dev/null || true)" = true ]; then
    if ! docker compose --project-directory /opt/desklink-relay -f /opt/desklink-relay/compose.yml restart relay; then
        if [ -n "$previous" ]; then
            set_current "$previous"
            docker compose --project-directory /opt/desklink-relay -f /opt/desklink-relay/compose.yml restart relay || true
        fi
        exit 1
    fi
fi
