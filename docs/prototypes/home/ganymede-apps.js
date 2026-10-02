// Read from ganymede's XDG_DATA_DIRS on 2026-10-02, NoDisplay/Hidden/Type!=Application already dropped.
window.GANYMEDE_APPS = [
 {
  "id": "btop.desktop",
  "name": "btop++",
  "icon": "btop",
  "cats": "System;Monitor;ConsoleOnly;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "btop"
 },
 {
  "id": "com.moonlight_stream.Moonlight.desktop",
  "name": "Moonlight",
  "icon": "moonlight",
  "cats": "Qt;Game;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "moonlight"
 },
 {
  "id": "firefox.desktop",
  "name": "Firefox",
  "icon": "firefox",
  "cats": "Network;WebBrowser",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "firefox --name firefox %U"
 },
 {
  "id": "kdesystemsettings.desktop",
  "name": "KDE System Settings",
  "icon": "preferences-system",
  "cats": "Qt;KDE;Settings;",
  "onlyshowin": "",
  "notshowin": "KDE;",
  "exec": "systemsettings"
 },
 {
  "id": "neovide.desktop",
  "name": "Neovide",
  "icon": "neovide",
  "cats": "Utility;TextEditor;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "neovide %F"
 },
 {
  "id": "nvim.desktop",
  "name": "Neovim",
  "icon": "nvim",
  "cats": "Utility;TextEditor;Development;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "nvim %F"
 },
 {
  "id": "org.kde.kdeconnect.app.desktop",
  "name": "KDE Connect",
  "icon": "kdeconnect",
  "cats": "Qt;KDE;Network",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "kdeconnect-app"
 },
 {
  "id": "org.kde.kdeconnect.nonplasma.desktop",
  "name": "KDE Connect Indicator",
  "icon": "kdeconnect",
  "cats": "Qt;KDE;Network;",
  "onlyshowin": "",
  "notshowin": "KDE;",
  "exec": "kdeconnect-indicator"
 },
 {
  "id": "org.kde.kdeconnect.sms.desktop",
  "name": "KDE Connect SMS",
  "icon": "kdeconnect",
  "cats": "Qt;KDE;Network;InstantMessaging",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "kdeconnect-sms"
 },
 {
  "id": "ssh-jump.desktop",
  "name": "ssh jump",
  "icon": "",
  "cats": "",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "/nix/store/smn5bv5gqz8sfyg6c2rga82g8s6bd1m5-ghostty-1.3.1/bin/ghostty -e /nix/store/51yi7b64mxyspaa012c5gwpmg92rs5c5-zsh-5.9.2/bin/zsh -c /nix/store/ncayd9znr6bdr94wr0zghjb91x6qi29y-ssh_jump/bin/ssh_jump"
 },
 {
  "id": "ssh-luna.desktop",
  "name": "ssh luna",
  "icon": "",
  "cats": "",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "/nix/store/smn5bv5gqz8sfyg6c2rga82g8s6bd1m5-ghostty-1.3.1/bin/ghostty -e /nix/store/51yi7b64mxyspaa012c5gwpmg92rs5c5-zsh-5.9.2/bin/zsh -c /nix/store/9idy7g1k7slx08gsi2q66425d40l4x6j-ssh_luna/bin/ssh_luna"
 },
 {
  "id": "ssh-remote_luna.desktop",
  "name": "ssh remote_luna",
  "icon": "",
  "cats": "",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "/nix/store/smn5bv5gqz8sfyg6c2rga82g8s6bd1m5-ghostty-1.3.1/bin/ghostty -e /nix/store/51yi7b64mxyspaa012c5gwpmg92rs5c5-zsh-5.9.2/bin/zsh -c /nix/store/vzinwvvipap685ac4485ywkr28ijpjf6-ssh_remote_luna/bin/ssh_remote_luna"
 },
 {
  "id": "systemsettings.desktop",
  "name": "System Settings",
  "icon": "preferences-system",
  "cats": "Qt;KDE;Settings;",
  "onlyshowin": "KDE;",
  "notshowin": "",
  "exec": "systemsettings"
 },
 {
  "id": "yazi.desktop",
  "name": "Yazi File Manager",
  "icon": "yazi",
  "cats": "System;FileManager;FileTools;ConsoleOnly",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "yazi %f"
 },
 {
  "id": "nixos-manual.desktop",
  "name": "NixOS Manual",
  "icon": "nix-snowflake",
  "cats": "System",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "nixos-help"
 },
 {
  "id": "nvidia-settings.desktop",
  "name": "NVIDIA X Server Settings",
  "icon": "nvidia-settings",
  "cats": "Settings",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "nvidia-settings"
 },
 {
  "id": "org.kde.ark.desktop",
  "name": "Ark",
  "icon": "ark",
  "cats": "Qt;KDE;Utility;Archiving;Compression;X-KDE-Utilities-File;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "ark %U"
 },
 {
  "id": "org.kde.discover.desktop",
  "name": "Discover",
  "icon": "plasmadiscover",
  "cats": "Qt;KDE;System;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "plasma-discover %F"
 },
 {
  "id": "org.kde.dolphin.desktop",
  "name": "Dolphin",
  "icon": "org.kde.dolphin",
  "cats": "Qt;KDE;System;FileTools;FileManager;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "dolphin %u"
 },
 {
  "id": "org.kde.drkonqi.coredump.gui.desktop",
  "name": "Crashed Processes Viewer",
  "icon": "tools-report-bug",
  "cats": "Qt;KDE;System;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "drkonqi-coredump-gui"
 },
 {
  "id": "org.kde.elisa.desktop",
  "name": "Elisa",
  "icon": "elisa",
  "cats": "Qt;KDE;Audio;AudioVideo",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "elisa %U"
 },
 {
  "id": "org.kde.gwenview.desktop",
  "name": "Gwenview",
  "icon": "gwenview",
  "cats": "Qt;KDE;Graphics;Viewer;Photography;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "gwenview %U"
 },
 {
  "id": "org.kde.kate.desktop",
  "name": "Kate",
  "icon": "kate",
  "cats": "Qt;KDE;Development;TextEditor;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "kate -b %U"
 },
 {
  "id": "org.kde.khelpcenter.desktop",
  "name": "Help Center",
  "icon": "help-browser",
  "cats": "Qt;KDE;Core;Documentation;",
  "onlyshowin": "KDE;",
  "notshowin": "",
  "exec": "khelpcenter %u"
 },
 {
  "id": "org.kde.kinfocenter.desktop",
  "name": "Info Center",
  "icon": "hwinfo",
  "cats": "Qt;KDE;System;Documentation;",
  "onlyshowin": "KDE;",
  "notshowin": "",
  "exec": "kinfocenter"
 },
 {
  "id": "org.kde.kmenuedit.desktop",
  "name": "Menu Editor",
  "icon": "kmenuedit",
  "cats": "Qt;KDE;System;",
  "onlyshowin": "KDE;",
  "notshowin": "",
  "exec": "kmenuedit"
 },
 {
  "id": "org.kde.konsole.desktop",
  "name": "Konsole",
  "icon": "utilities-terminal",
  "cats": "Qt;KDE;System;TerminalEmulator;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "konsole"
 },
 {
  "id": "org.kde.kwalletmanager.desktop",
  "name": "KWalletManager",
  "icon": "kwalletmanager",
  "cats": "Qt;KDE;System;Security;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "kwalletmanager5 %F"
 },
 {
  "id": "org.kde.kwrite.desktop",
  "name": "KWrite",
  "icon": "kwrite",
  "cats": "Qt;KDE;Utility;TextEditor;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "kwrite %U"
 },
 {
  "id": "org.kde.okular.desktop",
  "name": "Okular",
  "icon": "okular",
  "cats": "Qt;KDE;Graphics;Office;Viewer;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "okular %U"
 },
 {
  "id": "org.kde.plasma-systemmonitor.desktop",
  "name": "System Monitor",
  "icon": "utilities-system-monitor",
  "cats": "Qt;KDE;System;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "plasma-systemmonitor"
 },
 {
  "id": "org.kde.plasma.emojier.desktop",
  "name": "Emoji Selector",
  "icon": "preferences-desktop-emoticons",
  "cats": "Qt;KDE;Utility;",
  "onlyshowin": "KDE;",
  "notshowin": "",
  "exec": "plasma-emojier"
 },
 {
  "id": "org.kde.qrca.desktop",
  "name": "Qrca",
  "icon": "org.kde.qrca",
  "cats": "Qt;KDE;Utility;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "qrca"
 },
 {
  "id": "org.kde.spectacle.desktop",
  "name": "Spectacle",
  "icon": "spectacle",
  "cats": "Qt;KDE;Utility;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "/nix/store/1mzxp618dxv8934d263ddw75ai99y0mi-spectacle-6.7.5/bin/spectacle"
 },
 {
  "id": "steam.desktop",
  "name": "Steam",
  "icon": "steam",
  "cats": "Network;FileTransfer;Game;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "steam %U"
 },
 {
  "id": "xterm.desktop",
  "name": "XTerm",
  "icon": "xterm-color_48x48",
  "cats": "System;TerminalEmulator;",
  "onlyshowin": "",
  "notshowin": "",
  "exec": "xterm"
 }
];
