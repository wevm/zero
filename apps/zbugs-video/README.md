# A video of the four zbugs apps

`record.ts` records zbugs' four native apps side by side on one screen,
against one backend: zbugs-swift on macOS, [zbugs-gpui](../zbugs-gpui),
[zbugs-rune](../zbugs-rune), and zbugs-swift on an iPhone simulator. One
cursor and keyboard rotate the storyboard's steps across the apps, so each
change shows up live in the other three:

1. Rune creates an issue; the others open it.
2. The iPhone comments.
3. GPUI closes it.
4. Swift on the Mac answers.
5. Rune labels it, then lists closed issues.
6. The iPhone reopens it.
7. GPUI renames it.
8. Each app scrolls fast through its list of 20,000 issues.

Each step's caption goes on afterwards, in the space beside the phone. Then
Postgres must have each change. A take runs about two minutes.

The windows tile the main screen two by two. A screen too small for that
(each cell needs zbugs-rune's smallest window, 900×520 points), such as a
MacBook's built-in screen at its default 1512×982, switches to its largest
HiDPI mode for the take (1800×1169 there) with `displayplacer`, then back;
`ZBUGS_VIDEO_DISPLAY=keep` leaves it alone. The screen is captured whole at
60 fps (ffmpeg's AVFoundation input, with the cursor and its clicks) and
encoded at up to 3840 wide.

## Running it

It needs macOS with Xcode (26 or later), Rust, Node 24, `psql`, and:

```sh
brew install ffmpeg cliclick xcodegen displayplacer
```

The app that runs it, such as Terminal or the Amp runner's terminal, needs
two permissions in System Settings → Privacy & Security, which macOS asks for
the first time:

- **Accessibility**, for `cliclick` to move the cursor and type, and for
  `ax` (`ax.swift`) to find the apps' controls and place their windows.
- **Screen & System Audio Recording**, for ffmpeg to capture the screen.

Quit and reopen that app after granting them. Then start zbugs' backend, as
for zbugs-swift (see `apps/zbugs-swift/README.md`), which prints the crew
token:

```sh
ZBUGS_PG=postgresql://user:password@127.0.0.1:6434/postgres \
  node apps/zbugs-swift/server/server.ts
```

and record:

```sh
ZBUGS_TOKEN=<token> node apps/zbugs-video/record.ts
```

That builds the apps (`build`), adds 20,000 issues from zbugs' synthetic
data generator to the zero project, once (`seed`, which needs `ZBUGS_DB_URL`
when the database isn't the backend's default `zbugs_swift` on
127.0.0.1:5432), launches the apps and tiles them (`launch`), and records
(`record`); each runs alone too. The video, its raw take, and the apps' local
stores are in `~/.zbugs-video` (`ZBUGS_VIDEO_DIR`). Leave the mouse and
keyboard alone while it records. `record.ts quit` quits the apps it
launched.

A take that fails stops at the step that did and says which element it
couldn't find; `ax dump <pid>` (in `~/.zbugs-video`) prints what an app
exposes.
