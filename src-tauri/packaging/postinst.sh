#!/bin/sh
# Registers the syncparty:// scheme handler.
#
# The .desktop file carries MimeType=x-scheme-handler/syncparty, but nothing
# routes a link to it until update-desktop-database has rebuilt mimeinfo.cache.
# On most desktops `desktop-file-utils` ships a dpkg trigger that does this
# already; this is for the installs that do not have it, where an invite link
# would otherwise open nothing at all and give no hint why.
#
# Failure is not fatal: the app is installed and works, only the one-click
# invite does not, and refusing the whole installation over that would be a
# worse trade.
set -e

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi

exit 0
