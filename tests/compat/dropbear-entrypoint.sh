#!/bin/sh
set -eu
mkdir -p /home/fixture/.ssh /etc/dropbear
chmod 700 /home/fixture/.ssh
cp /fixtures/client.pub /home/fixture/.ssh/authorized_keys
chmod 600 /home/fixture/.ssh/authorized_keys
chown -R fixture:fixture /home/fixture/.ssh
dropbearkey -t ed25519 -f /etc/dropbear/fixture_ed25519
exec /usr/sbin/dropbear -r /etc/dropbear/fixture_ed25519 -F -E -w -p 22
