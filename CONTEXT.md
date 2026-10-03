# emrakul

A TV appliance: a PC on a TV that shows one thing at a time and is driven from the couch with a Steam Controller.

## Language

**Home**:
The launcher screen, in the style of webOS or SteamOS: every app, most recently used first. Going Home ends the running app; the Steam button always goes Home.
_Avoid_: Launcher, dashboard, desktop

**App**:
Anything Home can launch: an installed desktop entry. At most one app runs at a time. A web app and a game are both apps.
_Avoid_: Shortcut, tile, program

**Web app**:
An app that is a browser (Firefox or Chromium) opening one URL fullscreen, such as YouTube or Jellyfin.
_Avoid_: Bookmark, site, browser shortcut

**Game**:
An app that streams one game from the gaming desktop over Moonlight. Games are never run on the TV machine itself.
_Avoid_: Stream, Moonlight app

**Gaming desktop**:
The machine games are streamed from (saturn), running Sunshine.
_Avoid_: Host, server

**Idle**:
No activity for the idle timeout while nothing that counts is inhibiting. Activity is a button, trigger or trackpad touch, a stick moved past its deadzone, the controller switching on, or a key press; gyro never counts. A web app playing video holds off idle; a game never does, so a forgotten paused game still goes idle.
_Avoid_: Inactive, away

**Blanked**:
The state the TV machine enters when idle: the screen is switched off, and the TV may power itself down after a while. The running app keeps running, and the next activity brings back whatever was there. The input that wakes it does nothing else.
_Avoid_: Screensaver, sleep, suspend

**On-screen keyboard**:
The keyboard emrakul draws over a running app when Menu is pressed. What you pick on it is typed into the app as if on a real keyboard. While it is open, the controller drives it and nothing else reaches the app.
_Avoid_: OSK (in prose), virtual keyboard, text input
