#!/bin/sh
set -eu
mkdir -p /run/sshd /home/fixture/.ssh
chmod 700 /home/fixture/.ssh
cp /fixtures/client.pub /home/fixture/.ssh/authorized_keys
chmod 600 /home/fixture/.ssh/authorized_keys
chown -R fixture:fixture /home/fixture/.ssh
ssh-keygen -A
exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
