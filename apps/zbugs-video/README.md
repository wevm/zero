# A video of the five zbugs apps

`record.ts` records five zbugs apps side by side on one screen, against one
backend: zbugs-swift on macOS, [zbugs-gpui](../zbugs-gpui),
[zbugs-rune](../zbugs-rune), [zbugs](../zbugs) on the web in Chrome, and
zbugs-swift on an iPhone simulator. All five log in with the backend's crew
token. One cursor and keyboard rotate the storyboard's steps
(`storyboard.ts`) across the apps, so each change shows up live in the other
four:

1. Rune creates an issue; the others open it.
2. The iPhone comments.
3. The browser closes it.
4. Swift on the Mac answers.
5. Rune labels it, then lists closed issues.
6. The iPhone reopens it.
7. GPUI renames it.
8. The browser comments.
9. Each app scrolls fast through its list of 20,000 issues.

The iPhone shows in Xcode 26's Simulator or, with Xcode 27, in Device Hub,
which replaces it. Steps that can drop a click retry until their change
shows, and a step whose change never shows stops the take.

Each step's caption goes on afterwards, in a band under the screen. Then
Postgres must have each change. A take runs about two minutes.

The windows tile the main screen in two rows: Swift and GPUI on top; Rune,
the web, and the iPhone, as tall as the row, below. A screen too small for
that (each half needs zbugs-rune's smallest window, 900×520 points), such as
a MacBook's built-in screen at its default 1512×982, switches to its largest
HiDPI mode for the take (1800×1169 on a 14-inch MacBook Pro) with
`displayplacer`, then back; `ZBUGS_VIDEO_DISPLAY=keep` leaves it alone. The
web app zooms out (Chrome's Cmd-minus) until its cell is wider than 900 CSS
pixels, where it keeps its sidebar. The screen is captured whole at 60 fps
(ffmpeg's AVFoundation input, with the cursor and its clicks) and encoded at
up to 3840 wide.

## Running it

It needs macOS with Xcode (26 or later), Rust, Node 24, `psql`, Google
Chrome, and:

```sh
brew install ffmpeg cliclick xcodegen displayplacer
```

The app that runs it, such as Terminal or the Amp runner's terminal, needs
two permissions in System Settings → Privacy & Security, which macOS asks for
the first time:

- **Accessibility**, for `cliclick` to move the cursor and type, and for
  `ax` (`ax.swift`) to find the apps' controls and place their windows.
- **Screen & System Audio Recording**, for ffmpeg to capture the screen.

Quit and reopen that app after granting them; `~/.zbugs-video/ax
permissions` says whether it has them, once `build` has run. Then start
zbugs' backend, as for zbugs-swift (see `apps/zbugs-swift/README.md`), with
`ZBUGS_WEB=1` so it serves the web app's build too; it prints the crew token:

```sh
ZBUGS_WEB=1 ZBUGS_PG=postgresql://user:password@127.0.0.1:6434/postgres \
  node apps/zbugs-swift/server/server.ts
```

A backend on other ports needs `ZBUGS_CACHE_URL` and `ZBUGS_API_URL` for
`record.ts` too; the web app is built against `ZBUGS_CACHE_URL`.

and record:

```sh
ZBUGS_TOKEN=<token> node apps/zbugs-video/record.ts
```

That builds the apps, the web app into `apps/zbugs/dist` (`build`), adds 20,000 issues from zbugs' synthetic
data generator to the zero project, once (`seed`, which needs `ZBUGS_DB_URL`
when the database isn't the backend's default `zbugs_swift` on
127.0.0.1:5432), launches the apps and tiles them (`launch`), and records
(`record`); each runs alone too. Chrome runs with a profile of its own in
`~/.zbugs-video/chrome`, logged in through the DevTools protocol. The video, its raw take, and the apps' local
stores are in `~/.zbugs-video` (`ZBUGS_VIDEO_DIR`). Leave the mouse and
keyboard alone while it records. `record.ts quit` quits the apps it
launched.

A take that fails stops at the step that did and says which element it
couldn't find; `ax dump <pid>` (in `~/.zbugs-video`) prints what an app
exposes, and `ax text <pid>` the text on its screen.

## A real iPhone

With `ZBUGS_VIDEO_DEVICE` set to an iPhone's identifier (`xcrun devicectl
list devices`), the iPhone in the video is that one, shown by iPhone
Mirroring, in place of the simulator. It needs:

- zbugs-swift installed on the iPhone, built from Xcode with your team.
- The backend reachable from it: start the backend with `ZBUGS_HOST=0.0.0.0`.
  The app reaches it at the Mac's address on en0, or at
  `ZBUGS_VIDEO_DEVICE_HOST`.
- iPhone Mirroring unlocked on the Mac, with the iPhone locked.

`launch` starts the app logged in through `devicectl`, and places the
Mirroring window, which keeps its own size, in the phone's cell. Mirroring
exposes none of the app's controls, so the phone's steps find what they tap
in the text on its screen, read by macOS's text recognition. Back and
reopen are icons, tapped at their place on the screen. Each phone step is
checked where its change lands, in Swift on the Mac or on the phone's
screen, before the take goes on.
