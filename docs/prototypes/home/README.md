# PROTOTYPE: Home layouts (throwaway)

Static mocks for [What does Home look like, and how do you move through it?](https://github.com/cramt/emrakul/issues/7).
Three layouts (`ribbon`, `grid`, `list`), each with recency and with nothing launched yet.

- `ganymede-apps.js` is the real desktop-entry list from ganymede on 2026-10-02 (NoDisplay, Hidden and non-Application entries dropped). OnlyShowIn=KDE entries are dropped in the page.
- Entries tagged PLANNED are not installed yet: stand-ins for the web apps and games nixconf will declare. Game art is Steam's library capsule and hero images.
- `./render.sh` re-renders the six 3840x2160 PNGs. Open `index.html?v=grid&state=empty` to view one in a browser.
