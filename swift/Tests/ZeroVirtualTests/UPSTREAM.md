# zero-virtual tests in ZeroVirtual

`ZeroVirtual` is a native counterpart of
[`@rocicorp/zero-virtual`](https://github.com/rocicorp/zero-virtual), not a
port of its code. Its behavior tests are ported by hand from the upstream
tests at commit
[`f89c468`](https://github.com/rocicorp/zero-virtual/commit/f89c4685b748fb80fba1077c8c9bcdb29b73326e),
keeping their names, with the DOM replaced by a fake scroll view
(`Harness.swift`) whose geometry the test sets.

When zero-virtual changes, diff its tests from this commit and update this
list.

- **ported**: same steps and expectations.
- **adapted**: same behavior, set up or checked the native way (noted).
- **n/a**: a feature ZeroVirtual doesn't have, or a browser detail with no
  native counterpart (noted).

## `src/core/rows.test.ts` → `RowsTests.swift`

| Test                                                              | Status                                                                  |
| ----------------------------------------------------------------- | ----------------------------------------------------------------------- |
| %s pages exclude the sentinel and retain query order              | ported                                                                  |
| permalink pages reserve the center row and exclude both sentinels | ported                                                                  |
| a lone permalink retains the existing empty-window semantics      | ported                                                                  |
| row lookup stays lazy for mutable query arrays                    | n/a: Swift arrays are values, so there is nothing to see a later change |

## `src/solid/create-rows.test.ts` → `RowsTests.swift`

| Test                                                   | Status                                                                                                                                               |
| ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| forward anchor: main query is built and rows assemble  | adapted: "a page anchor asks for one row more than the page"                                                                                         |
| permalink anchor: stages queries 2/3 on query 1 result | adapted: "a permalink looks its row up, then pages either side of it", and `ZeroVirtualizerTests.opensAtAPermalinkWithAPageEitherSide` over a client |

## `src/core/virtualizer.test.ts` → `VirtualizerTests.swift`

| Test                                                                  | Status                                                               |
| --------------------------------------------------------------------- | -------------------------------------------------------------------- |
| ZeroVirtualizer snapshot — items, space and total (6 cases)           | ported                                                               |
| items carry the correct index and row                                 | ported                                                               |
| explicit count overrides estimated total                              | n/a: no `count` (an estimate is enough)                              |
| explicit zero count reports empty list                                | n/a: no `count`                                                      |
| items survive metadata updates but change with row keys and data      | n/a: snapshots are values; SwiftUI diffs by key                      |
| attachment cleanup runs once per container and restores its style     | n/a: there are no DOM styles or observers to restore                 |
| changing count invalidates the cached snapshot                        | n/a: no `count`                                                      |
| changing estimateSize invalidates the cached snapshot                 | ported                                                               |
| scrolling notifies the settled → false transition                     | ported                                                               |
| snapshot identity is cached until state changes                       | n/a: snapshots are values                                            |
| setRows/setOptions are silent; afterDOMUpdate notifies on transitions | ported (`afterUpdate`)                                               |
| warns once when listContextParams churns identity without content     | n/a: list contexts compare by value, so there is no churn to warn of |
| identity-churn warning is best-effort for non-serializable params     | n/a: as above                                                        |
| query inputs fall back to the new context after a context change      | ported                                                               |
| lanes (grid): all 5 tests                                             | n/a: no grids yet                                                    |
| masonry: both tests                                                   | n/a: no masonry yet                                                  |
| minPageSize: all 3 tests                                              | ported                                                               |

## `src/core/virtualizer.dom.test.ts` → `ScrollTests.swift`

| Test                                                                               | Status                                                                                                                                |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| **paging against a real (fake-geometry) scroll container**                         |                                                                                                                                       |
| initial load anchors at the top and fills one page                                 | ported                                                                                                                                |
| scrolling near the window end advances the forward anchor                          | ported                                                                                                                                |
| scrolling back near the window start pages backward                                | ported                                                                                                                                |
| a far jump into unloaded space below recovers by cascading pages                   | adapted: the space below comes from having seen the rows, not `count`                                                                 |
| a far jump back to the very top re-anchors at the start                            | adapted: as above                                                                                                                     |
| a page too small for what is on screen grows instead of paging back and forth      | ported                                                                                                                                |
| **manual scroll anchoring**                                                        |                                                                                                                                       |
| growth above the anchor row is compensated into scrollTop                          | ported                                                                                                                                |
| shrinkage above the anchor row is compensated too                                  | ported                                                                                                                                |
| growth at scroll offset 0 is revealed, not compensated away                        | ported                                                                                                                                |
| growth below the anchor row does not move the viewport                             | ported                                                                                                                                |
| **a grid (lanes)**: all 4 tests                                                    | n/a: no grids yet                                                                                                                     |
| **scroll-state restore when the container mounts lazily**                          |                                                                                                                                       |
| restores the saved scrollTop when the element attaches on a later commit           | ported                                                                                                                                |
| does not persist a spurious scrollTop:0 while detached                             | ported                                                                                                                                |
| **reporting the scroll state**                                                     |                                                                                                                                       |
| every scroll is reported as it happens, not once the scroll ends                   | ported                                                                                                                                |
| **permalink scroll**                                                               |                                                                                                                                       |
| scrolls the permalink row into view when its row key differs from the permalink id | ported                                                                                                                                |
| %s anchoring: the row stays on top when the page grows under it                    | ported                                                                                                                                |
| the jump holds on while the rows before its target are still on their way          | adapted: `viewportInsets` for the CSS `scroll-padding-top`                                                                            |
| **scroll-state restore of a position we wrote**                                    |                                                                                                                                       |
| a later navigation back to it restores it                                          | adapted: `restore(_:)`, since a value can't be told apart by identity; also restores the same state twice                             |
| from a host that hands every write straight back: scrolling                        | ported                                                                                                                                |
| from a host that hands every write straight back: a jump                           | ported                                                                                                                                |
| **a restore that moves the loaded window**                                         |                                                                                                                                       |
| manual anchoring lands it where it was saved (first answer missing %i rows)        | ported                                                                                                                                |
| %s anchoring: a position the outgoing window was too short to reach still lands    | ported                                                                                                                                |
| %s anchoring: paging leaves the restored window alone until its rows are in        | ported                                                                                                                                |
| **compareStartRows**                                                               |                                                                                                                                       |
| a start row JSON cannot serialize survives the round trip                          | n/a: start rows are `Equatable`, compared with `==`                                                                                   |
| is what makes that work: without it the compare throws                             | n/a: as above                                                                                                                         |
| or narrow the start row in toStartRow: the anchor carries what it returns          | adapted: a narrower `Codable` start row survives saving                                                                               |
| **firstVisibleItem / lastVisibleItem**                                             |                                                                                                                                       |
| report the edges of the viewport, not of the loaded window                         | ported                                                                                                                                |
| follow the scroll position                                                         | ported                                                                                                                                |
| count a row that overlaps the scrollport at all                                    | ported                                                                                                                                |
| carry the row and its key, which scrollToItem takes                                | adapted: a key goes to `scrollToRow(key:)`                                                                                            |
| are undefined with no scroll element attached                                      | ported                                                                                                                                |
| **no-op re-anchor does not spin the render loop**                                  | ported                                                                                                                                |
| **below-viewport window at the start of the list**                                 | ported                                                                                                                                |
| **scrollToItem**                                                                   |                                                                                                                                       |
| a loaded row scrolls immediately, without re-anchoring                             | ported                                                                                                                                |
| an unloaded row re-anchors and the scroll lands once its page renders              | ported                                                                                                                                |
| is level-triggered: the same id twice scrolls twice                                | ported                                                                                                                                |
| align: defaults to auto / auto leaves a visible row / start / center / end         | ported                                                                                                                                |
| aligns below the container's scroll-padding (a sticky header)                      | adapted: `viewportInsets`                                                                                                             |
| keeps the space the target row's scroll-margin asks for                            | n/a: rows have no per-row margin; insets cover sticky headers                                                                         |
| a clamped jump still releases the request, so paging keeps working                 | ported                                                                                                                                |
| a loaded row keyed differently from its id is scrolled to, not re-fetched          | ported                                                                                                                                |
| addressing a rendered row by its key: scrolls, and issues no lookup at all         | ported                                                                                                                                |
| addressing a rendered row by its key: takes a numeric key                          | n/a: keys are typed                                                                                                                   |
| addressing a rendered row by its key: reaches rendered rows only                   | ported, plus `scrollToRow(key:)`                                                                                                      |
| a repeat jump to a row keyed differently from its id still lands                   | ported                                                                                                                                |
| an empty id is a no-op, not a lookup that never answers                            | ported                                                                                                                                |
| a second jump to a rendered row supersedes a lookup still in flight                | ported                                                                                                                                |
| a second jump supersedes one that is still loading                                 | ported                                                                                                                                |
| an id that does not exist: all 5 tests                                             | ported                                                                                                                                |
| a permalink that does not exist leaves the list alone even mid-lookup              | ported                                                                                                                                |
| a permalinkID that does not exist leaves a loaded list alone                       | ported                                                                                                                                |
| a deep link to a permalinkID that does not exist falls back to the top             | ported                                                                                                                                |
| **attachment geometry**                                                            |                                                                                                                                       |
| alignment and visibility with window scrolling=%s                                  | adapted: insets at both ends ("alignment keeps clear of the strips covering both ends of the viewport"); there is no window scrolling |
| moving a container updates its geometry without a resize notification              | n/a: frames are in content coordinates                                                                                                |
| touch growth stays held until scrollend with window scrolling=%s                   | n/a: a browser workaround; native scroll views take offset changes mid-fling. Checked on a device instead                             |
| reattachment falls back to the new viewport until its observer reports             | adapted: "follow the viewport's size"                                                                                                 |

## Stick to bottom → `StickToBottomTests.swift`

| Upstream                                                              | Status                                                                             |
| --------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `src/react/use-zero-virtualizer.test.ts`: the `useStickToBottom` case | adapted: "parked at the bottom, it follows new rows and a last row growing taller" |
| `demo/react/e2e/tests/stick.spec.ts`                                  | adapted: "scrolled away, growth does not follow; back at the bottom, it re-arms"   |

## Not ported

- `src/core/virtualizer.masonry.dom.test.ts`: no masonry yet.
- `src/react/use-history-scroll-state.test.ts`,
  `src/solid/create-history-scroll-state.test.ts`: the Navigation API
  helpers. Apps save `onScrollStateChange`'s state themselves (zbugs-swift
  in `SceneStorage`).
- The rest of the React and Solid binding tests: framework wiring. The
  SwiftUI, UIKit, and AppKit adapters have their own tests.
- `demo/react/e2e`: the browser demo, other than `stick.spec.ts`.

## Swift's own

- `ZeroVirtualizerTests.swift`: the virtualizer over a client and a server
  the test plays: paging, a permalink, a jump, and a new list context.
- "rows of different heights page down and back without moving what is on
  screen", "paging works the same when the scroll view reports scrolls as
  they happen".
- "a window whose rows were all deleted pages back to the rows before it": a behavior upstream lacks. At `f89c468`, `#evaluatePaging` and `#updatePaging` in `src/core/virtualizer.ts` both skip an empty window, so a list scrolled away from its start whose rows from the window's start row on are deleted stays empty, and scrolling doesn't bring it back.
- "deleting the row a list opened at, or jumped to, leaves the rows around it on screen": a deliberate divergence. At `f89c468`, `#recoverFromMissingPermalink` in `src/core/virtualizer.ts` sends any list whose permalink lookup reports its row missing to the top, including a loaded list whose permalink or jump target was deleted after it landed. Here only a cold miss (a deep link to a row that never loaded) goes to the top. A loaded list pages forward from the first row still on screen, and the rows below the deleted row move up into its place, as they do for any other row deleted on screen.
- "at the bottom, content getting shorter than the offset needs takes the offset back", "a list that gets shorter than the viewport shows its rows", "stuck to the bottom, the viewport getting shorter keeps the bottom in view": no upstream test. The browser clamps the scroll position to the content and reports it as a scroll, and upstream relies on that. `UIScrollView` and `NSClipView` don't clamp it, so `LayoutHost` does.
- "a jump to a loaded row near the end of the loaded rows lines it up once rows load below": a deliberate divergence. At `f89c468`, `#land` in `src/core/virtualizer.ts` releases a jump once its window settles, even if the scroll was clamped short, so paging isn't held up ("a clamped jump still releases the request, so paging keeps working"). A row near the end of the loaded rows, with nothing seen below them, is then never lined up: the rows that load below make room, but nothing scrolls into it. Here the jump is still released, so paging carries on. If it was clamped short at the end of the loaded rows rather than the list's own end, it scrolls on to its row as rows load, until the row lines up, the list's end clamps it, or the user scrolls.
- "after scrolling up past the loaded rows, a row on screen growing moves only the rows below it": a deliberate divergence. Upstream's manual anchoring picks its reference row (`#refreshAnchor`) only at a scroll, a landing, and a reset. A scroll into the space above the loaded rows picks their first row, which is below the viewport. The page that loads above then pushes that row down the screen, and until the next scroll a row above it changing height moves the rows above it instead of the ones below. Here the reference is picked again whenever the rows are in place, as native scroll anchoring picks it on each layout.
- "scrolled up past the loaded rows of a list opened at a permalink, the next page is on screen": Swift's own. Upstream keeps no reserve of space above a list opened away from its start (`estimatedSpaceBefore` here), so its pages go where their indexes estimate them. Here a page that shares no row with the last one goes where the space estimated its rows, taken from the space as it was rather than from the reserve. Before, it stood under the reserve where the old rows were, below the viewport, and the list paged on to its start and back.
- "scrolled past the loaded rows of rows shorter than estimated, the next page is on screen": Swift's own, the forward side of the one above. Paging forward, the rows passed are the last page's, laid out, so the next page goes where they end, not where their estimate does.
- "deleting the row a list shorter than a page opened at, the list settles on all its rows": a deliberate divergence. At `f89c468`, `updateAnchorForEdge` in `#evaluatePaging` (`src/core/virtualizer.ts`) bounds a forward page's start row to the window's first row, and the start row is exclusive, so a window at the list's start that pages forward from its first row loses that row. With all the list in one page, the next page back loses the last row, and the window flips between the two. Here a forward page from the first row of a window at the list's start is the page from the start.
- "stuck to the bottom, back down to a short page at the end, the list settles there" and "a backward page from no start row is the list's end, and saves as one": a deliberate divergence, with an API change. At `f89c468`, a backward anchor always has a start row (`startRow: TStartRow` in `src/core/types.ts`), so a window at the list's end that pages backward from its last row loses that row. Stuck to the bottom, the space estimated for it moves the bottom, the list follows it, pages forward to the end again, and flips. Here `VirtualAnchor.backward`'s `startRow` is optional, nil for the page that ends at the list's end, which a page query gets as a backward request with no `start`, and a backward page from the last row of a window at the list's end is that page.
- "a thumb drag to the top past rows shorter than estimated shows no gap above the first row": Swift's own. Upstream has no space of its own before the rows to take out at the start (`spaceAdjust` here). Space left there that the offset near the top can't take, as when the scroll thumb is dragged up into the space above the rows and the start's page comes in, used to stand above the first row until the list rested. It now goes at once, and the rows below it come up to the top.
