/**
 * What record.ts records: nine steps, each one app changing the same issue
 * with the real cursor and keyboard while the other four show the change.
 * Targets are found by name through the accessibility API (ax.swift):
 * zbugs-rune exposes each element's key, zbugs-swift its labels, on macOS
 * and through the simulator alike, and Chrome the web app's roles and text.
 * zbugs-gpui exposes none, so its targets are its layout's: the 250-point
 * sidebar, the 380-point list, and the 52-point toolbars
 * (apps/zbugs-gpui/src/app.rs).
 */
import {execFileSync} from 'node:child_process';

export type Apps = {
  rune: number;
  gpui: number;
  swift: number;
  web: number;
  sim: number;
};

/** The issue the storyboard makes, as Rune creates it and GPUI renames it. */
export const TITLE = 'Live in all five apps';
const RENAMED = 'Live in all five apps, renamed in GPUI';
const DESCRIPTION = 'Created in Rune, synced by Zero.';
const IPHONE_COMMENT = 'Commented on the iPhone.';
const MAC_COMMENT = 'Answered from the Mac.';
const WEB_COMMENT = 'Signed off in the browser.';
const LABEL = 'bug';

type Rect = {x: number; y: number; w: number; h: number};
type Point = {x: number; y: number};
type Step = {
  title: string;
  detail: string;
  run: (apps: Apps) => Promise<void>;
};

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));
const center = (r: Rect): Point => ({x: r.x + r.w / 2, y: r.y + r.h / 2});

/**
 * `mirrored`: the iPhone is a real one in iPhone Mirroring, whose window
 * exposes no controls, so its steps find and check text on its screen
 * (`ax text`) and tap by position.
 */
export function storyboard(ax: string, mirrored = false) {
  const call = (args: string[]) =>
    execFileSync(ax, args, {encoding: 'utf8'}).trim();
  const rect = (s: string): Rect => {
    const [x, y, w, h] = s.split(' ').map(Number);
    return {x, y, w, h};
  };
  const window = (pid: number) => rect(call(['window', String(pid)]));
  const tryFind = (pid: number, query: string, index = 0) => {
    try {
      return rect(call(['find', String(pid), query, String(index)]));
    } catch {
      return undefined;
    }
  };
  /** Waits up to 15 seconds for an element, as for a change to sync. */
  async function find(pid: number, query: string, index = 0) {
    const deadline = Date.now() + 15_000;
    for (;;) {
      const r = tryFind(pid, query, index);
      if (r) return r;
      if (Date.now() > deadline) {
        throw new Error(`no element ${query} in ${pid}`);
      }
      await sleep(250);
    }
  }

  /** The window's line of text on screen that contains `text` (`=text`: is it). */
  const tryText = (pid: number, text: string) => {
    try {
      return rect(call(['text', String(pid), text]));
    } catch {
      return undefined;
    }
  };
  async function waitText(pid: number, text: string) {
    const deadline = Date.now() + 15_000;
    for (;;) {
      const r = tryText(pid, text);
      if (r) return r;
      if (Date.now() > deadline) throw new Error(`no text "${text}" in ${pid}`);
      await sleep(250);
    }
  }
  /**
   * Acts until `done`, up to three times, 5 seconds apart: a click right
   * after an app comes to the front can be dropped. The take stops when it
   * never is, rather than going on from the wrong state.
   */
  async function until(
    what: string,
    act: () => Promise<unknown>,
    done: () => boolean,
  ) {
    for (let i = 0; i < 3; i++) {
      if (done()) return;
      await act();
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {
        if (done()) return;
        await sleep(250);
      }
    }
    throw new Error(`${what} didn't happen`);
  }

  const cliclick = (...args: string[]) =>
    execFileSync('cliclick', args, {stdio: 'ignore'});
  /** A glide to the point, then a press and release a hand would make. */
  async function click(p: Point) {
    const at = `${Math.round(p.x)},${Math.round(p.y)}`;
    cliclick('-e', '250', `m:${at}`);
    cliclick('w:120', `dd:${at}`, 'w:90', `du:${at}`);
    await sleep(350);
  }
  async function type(text: string) {
    cliclick('-w', '45', `t:${text}`);
    await sleep(300);
  }
  async function key(name: string, mods = '') {
    if (mods) cliclick(`kd:${mods}`, `kp:${name}`, `ku:${mods}`);
    else cliclick(`kp:${name}`);
    await sleep(300);
  }
  async function selectAll() {
    cliclick('kd:cmd', 't:a', 'ku:cmd');
    await sleep(200);
  }
  /**
   * Types into a field, again until it has the text: keys typed right
   * after an app comes to the front can be dropped.
   */
  async function fill(pid: number, field: string, text: string) {
    await until(
      `typing into ${field}`,
      async () => {
        await click(center(await find(pid, field)));
        await selectAll();
        await type(text);
      },
      () => !!tryFind(pid, `${field}&value=${text}`),
    );
  }
  /** Brings the app's window to the front, so the first click lands. */
  async function activate(pid: number) {
    execFileSync('osascript', [
      '-e',
      `tell application "System Events" to set frontmost of (first process whose unix id is ${pid}) to true`,
    ]);
    await sleep(500);
  }
  /** Opens a menu and picks from it, pressing again if the menu didn't open. */
  async function pick(pid: number, trigger: string, option: string, index = 0) {
    for (let i = 0; ; i++) {
      await click(center(await find(pid, trigger)));
      for (let j = 0; j < 8; j++) {
        const o = tryFind(pid, option, index);
        if (o) return click(center(o));
        await sleep(250);
      }
      if (i === 2) throw new Error(`${trigger} didn't open ${option}`);
    }
  }
  /**
   * Rune's list at its open or closed issues, from its status menu, picked
   * again until the menu shows it: a click on the menu can be dropped, and
   * leave it open.
   */
  async function runeStatus(apps: Apps, value: 'Open' | 'Closed') {
    const option = value === 'Open' ? 'id=option0' : 'id=option1';
    await until(
      `Rune listing ${value.toLowerCase()} issues`,
      async () => {
        const open = tryFind(apps.rune, option);
        if (open) await click(center(open));
        else await pick(apps.rune, 'id=status-trigger', option);
      },
      () => !!tryFind(apps.rune, `id=status-trigger&value==${value}`),
    );
  }
  /** A fast swipe down the list at `p`, then back up to its top. */
  async function scroll(p: Point) {
    cliclick('-e', '250', `m:${Math.round(p.x)},${Math.round(p.y)}`);
    await sleep(300);
    for (let i = 0; i < 5; i++) {
      call(['scroll', '-80', '60']);
      await sleep(250);
    }
    await sleep(600);
    call(['scroll', '400', '80']);
    await sleep(800);
  }

  // zbugs-gpui's layout, from its window's corner.
  const gpui = {
    detail: (apps: Apps) => window(apps.gpui).x + 250 + 380,
    toolbar: (apps: Apps) => window(apps.gpui).y + 26,
    /** The detail toolbar's buttons: close or reopen, edit, delete. */
    button: (apps: Apps, i: number) => ({
      x: gpui.detail(apps) + 23 + i * 27,
      y: gpui.toolbar(apps),
    }),
    /**
     * The edit dialog's Save: 640 wide, centered, a tenth of the window
     * down (gpui-component's Dialog), Enter doesn't press it.
     */
    save: (apps: Apps) => {
      const w = window(apps.gpui);
      return {x: w.x + w.w / 2 + 276, y: w.y + w.h / 10 + 339};
    },
    /** That dialog's close button, or a harmless spot without one. */
    closeDialog: (apps: Apps) => {
      const w = window(apps.gpui);
      return {x: w.x + w.w / 2 + 298, y: w.y + w.h / 10 + 21};
    },
    row: (apps: Apps, i: number) => ({
      x: window(apps.gpui).x + 250 + 190,
      y: window(apps.gpui).y + 52 + 28 + i * 56,
    }),
  };
  // The phone's screen, and its navigation bar's buttons there, which
  // neither the simulator nor iPhone Mirroring expose: back, then close or
  // reopen.
  // The wheel's way up the phone's list: positive in iPhone Mirroring and
  // Simulator, negative in Device Hub, as its fast scroll finds out.
  let wheelUp = 80;
  const phone = {
    /**
     * iPhone Mirroring's window, which keeps its size, stands for the
     * screen. Otherwise it's the first phone-shaped group but the window's
     * own: Simulator's first group is the screen, Device Hub's its whole
     * window, which can be phone-shaped too.
     */
    screen: async (apps: Apps): Promise<Rect> => {
      if (mirrored) return window(apps.sim);
      const deadline = Date.now() + 15_000;
      for (;;) {
        const w = window(apps.sim);
        for (let i = 0; ; i++) {
          const g = tryFind(apps.sim, 'role=AXGroup', i);
          if (!g) break;
          const own = g.x === w.x && g.y === w.y && g.w === w.w && g.h === w.h;
          if (!own && g.h > 1.9 * g.w) return g;
        }
        if (Date.now() > deadline) throw new Error('no phone screen');
        await sleep(250);
      }
    },
    at: async (apps: Apps, fx: number, fy: number) => {
      const s = await phone.screen(apps);
      return {x: s.x + fx * s.w, y: s.y + fy * s.h};
    },
    // In Mirroring's window, measured on an iPhone 15 Pro: its screen sits
    // lower in the window than the simulator's in its group.
    back: (apps: Apps) =>
      mirrored ? phone.at(apps, 0.116, 0.143) : phone.at(apps, 0.093, 0.096),
    toggleOpen: (apps: Apps) =>
      mirrored ? phone.at(apps, 0.752, 0.143) : phone.at(apps, 0.768, 0.096),
    /**
     * Which page the phone shows. Mirrored, from one read of its screen:
     * an issue says "Issue #", the filters "Zero Bugs" and "Sort", and the
     * list "Open Issues", or "Open Iss..." once scrolled. Its search field
     * can show only its magnifier, so it isn't the list's mark.
     */
    page: (apps: Apps): 'list' | 'issue' | 'filters' | 'other' => {
      if (!mirrored) {
        if (tryFind(apps.sim, 'id=IssueRow')) return 'list';
        if (tryFind(apps.sim, 'placeholder=Add a comment')) return 'issue';
        return tryFind(apps.sim, 'desc=Issues') ? 'filters' : 'other';
      }
      const text = call(['text', String(apps.sim)]).toLowerCase();
      if (text.includes('issue #')) return 'issue';
      if (text.includes('zero bugs') && text.includes('sort')) return 'filters';
      return text.includes('open iss') ? 'list' : 'other';
    },
    atList: (apps: Apps) => phone.page(apps) === 'list',
    atIssue: (apps: Apps) => phone.page(apps) === 'issue',
    /**
     * Back to the list: from the filters, to their issues; from anything
     * else, back. A tap the simulator takes for a long press opens the back
     * button's menu instead, which has the list too.
     */
    toList: (apps: Apps) =>
      until(
        'the iPhone going back to its list',
        async () => {
          if (phone.page(apps) === 'filters') {
            // Its "Issues" button, or where it is if its small text doesn't
            // read.
            const issues = mirrored
              ? tryText(apps.sim, '=issues')
              : tryFind(apps.sim, 'desc=Issues');
            await click(
              issues
                ? center(issues)
                : await phone.at(
                    apps,
                    mirrored ? 0.833 : 0.86,
                    mirrored ? 0.143 : 0.096,
                  ),
            );
            return;
          }
          await click(await phone.back(apps));
          await sleep(1000);
          const menu = mirrored
            ? undefined
            : tryFind(apps.sim, 'Open Issues&role=AXButton');
          if (menu) await click(center(menu));
        },
        () => phone.atList(apps),
      ),
    /**
     * Its list at the top: in the simulator, a tap on the status bar;
     * Mirroring passes that tap on to nothing, so its wheel, upwards until
     * "Open Issues" is the large title again.
     */
    toTop: async (apps: Apps) => {
      if (!mirrored) {
        await click(await phone.at(apps, 0.5, 0.03));
        await sleep(800);
        return;
      }
      const p = center(await phone.screen(apps));
      cliclick('-e', '250', `m:${Math.round(p.x)},${Math.round(p.y)}`);
      // The way that's up, then, if that doesn't get there, the other.
      for (let i = 0; i < 80 && !tryText(apps.sim, '=open issues'); i++) {
        if (i === 20) wheelUp = -wheelUp;
        call(['scroll', String(wheelUp), '60']);
        await sleep(400);
      }
      if (!tryText(apps.sim, '=open issues')) {
        throw new Error("the iPhone's list isn't at its top");
      }
    },
    /**
     * A fast swipe down its list, then back to the top. Device Hub turns the
     * wheel the other way round from Simulator, so a first swipe that leaves
     * the list where it was turns it round.
     */
    scroll: async (apps: Apps) => {
      const s = await phone.screen(apps);
      const p = center(s);
      cliclick('-e', '250', `m:${Math.round(p.x)},${Math.round(p.y)}`);
      await sleep(300);
      // The list's text, without the status bar's clock.
      const list = () =>
        call(['text', String(apps.sim)])
          .split('\n')
          .filter(l => Number(l.split(' ')[1]) > s.y + 0.15 * s.h)
          .join('\n');
      const before = list();
      let dy = -80;
      call(['scroll', String(dy), '60']);
      await sleep(500);
      if (list() === before) dy = -dy;
      wheelUp = -dy;
      for (let i = 0; i < 4; i++) {
        call(['scroll', String(dy), '60']);
        await sleep(250);
      }
      await sleep(600);
      await phone.toTop(apps);
    },
    open: async (apps: Apps, title: string) => {
      const r = mirrored
        ? await waitText(apps.sim, title)
        : await find(apps.sim, row(title));
      await until(
        'the iPhone opening the issue',
        () => click(center(r)),
        () => phone.atIssue(apps),
      );
    },
    comment: async (apps: Apps, text: string) => {
      await click(
        center(
          mirrored
            ? await waitText(apps.sim, 'a comment')
            : await find(apps.sim, 'placeholder=Add a comment'),
        ),
      );
      await type(text);
      await click(
        center(
          mirrored
            ? await waitText(apps.sim, '=comment')
            : await find(apps.sim, 'desc=Comment'),
        ),
      );
    },
  };
  const row = (title: string) => `id=IssueRow&desc=${title}`;
  // The web app's page, whose controls below the fold Chrome reports
  // without a height until they're scrolled into view.
  const web = {
    show: async (apps: Apps, query: string) => {
      const deadline = Date.now() + 15_000;
      for (;;) {
        try {
          return rect(call(['show', String(apps.web), query]));
        } catch (e) {
          if (Date.now() > deadline) throw e;
          await sleep(250);
        }
      }
    },
    row: (title: string) => `role=AXLink&title=${title}`,
    /** The sidebar's open issues, from any page. */
    toList: async (apps: Apps) => {
      await click(center(await find(apps.web, 'role=AXLink&title==Open')));
    },
  };

  const steps: Step[] = [
    {
      title: 'Rune creates an issue',
      detail: 'It lands at the top of the other four lists, and each opens it.',
      async run(apps) {
        await activate(apps.rune);
        await until(
          'Rune opening its new issue dialog',
          async () => click(center(await find(apps.rune, 'id=new-issue'))),
          () => !!tryFind(apps.rune, 'id=title-field'),
        );
        await type(TITLE);
        await click(center(await find(apps.rune, 'id=description-field')));
        await type(DESCRIPTION);
        await until(
          'Rune creating the issue',
          async () => {
            const save = tryFind(apps.rune, 'id=save');
            if (save) await click(center(save));
          },
          () => !tryFind(apps.rune, 'id=title-field'),
        );
        await activate(apps.swift);
        await click(center(await find(apps.swift, row(TITLE))));
        await activate(apps.gpui);
        await click(gpui.row(apps, 0));
        await activate(apps.web);
        await click(center(await find(apps.web, web.row(TITLE))));
        await activate(apps.sim);
        await phone.open(apps, TITLE);
      },
    },
    {
      title: 'The iPhone comments',
      detail: 'The comment shows in Swift, GPUI, Rune, and the browser.',
      async run(apps) {
        await phone.comment(apps, IPHONE_COMMENT);
        await find(apps.swift, `value=${IPHONE_COMMENT}`);
        await sleep(1500);
      },
    },
    {
      title: 'The browser closes it',
      detail:
        'Closed in all five apps, and gone from their lists of open issues.',
      async run(apps) {
        await activate(apps.web);
        // The menu's Closed, after the sidebar's.
        await pick(
          apps.web,
          'role=AXComboBox&value==open',
          'role=AXStaticText&value==closed',
          1,
        );
        await find(apps.swift, 'role=AXButton&desc=Reopen');
        await sleep(1500);
      },
    },
    {
      title: 'Swift on the Mac answers',
      detail: 'Its comment shows under the iPhone’s, in every app.',
      async run(apps) {
        await activate(apps.swift);
        await fill(
          apps.swift,
          'role=AXTextField&placeholder=comment',
          MAC_COMMENT,
        );
        await click(
          center(await find(apps.swift, 'role=AXButton&desc=Comment')),
        );
        await find(apps.rune, MAC_COMMENT);
        await sleep(1500);
      },
    },
    {
      title: 'Rune labels it, then lists closed issues',
      detail: `The ${LABEL} label shows in Swift and GPUI, and the issue is at the top of Rune's closed ones.`,
      async run(apps) {
        await activate(apps.rune);
        await pick(apps.rune, 'id=labels-trigger', `id=label-${LABEL}`);
        await key('esc');
        await sleep(400);
        await runeStatus(apps, 'Closed');
        await sleep(1500);
      },
    },
    {
      title: 'The iPhone reopens it',
      detail: "Open again everywhere: it leaves Rune's list of closed issues.",
      async run(apps) {
        await activate(apps.sim);
        await until(
          'the iPhone showing the issue',
          async () => {},
          () => phone.atIssue(apps),
        );
        const toggle = await phone.toggleOpen(apps);
        // Tapped again only while the issue still shows closed: on the
        // phone, mirrored (its change can take a moment to reach Swift);
        // otherwise in Swift.
        await until(
          'the iPhone reopening the issue',
          async () => {
            const closed = mirrored
              ? !tryText(apps.sim, 'open by')
              : !!tryFind(apps.swift, 'role=AXButton&desc=Reopen');
            if (closed) await click(toggle);
          },
          () => !!tryFind(apps.swift, 'role=AXButton&desc=Close Issue'),
        );
        await sleep(1500);
      },
    },
    {
      title: 'GPUI renames it',
      detail:
        'The new title shows in Swift, Rune, the browser, and on the iPhone.',
      async run(apps) {
        await activate(apps.gpui);
        await until(
          'GPUI opening its edit dialog',
          () => click(gpui.button(apps, 1)),
          () => !!tryText(apps.gpui, 'edit issue'),
        );
        await sleep(600);
        await selectAll();
        await type(RENAMED);
        await click(gpui.save(apps));
        await find(apps.swift, `value=${RENAMED}`);
        await sleep(1500);
      },
    },
    {
      title: 'The browser comments',
      detail: 'The third comment shows in the four native apps.',
      async run(apps) {
        await activate(apps.web);
        await click(center(await web.show(apps, 'role=AXTextArea')));
        await type(WEB_COMMENT);
        await click(
          center(await web.show(apps, 'role=AXButton&title==Add comment')),
        );
        await find(apps.swift, `value=${WEB_COMMENT}`);
        await sleep(1500);
      },
    },
    {
      title: 'Each app scrolls fast',
      detail:
        'Through hundreds of the 20,000 issues, its pages loading as it goes, then back to the top.',
      async run(apps) {
        await activate(apps.rune);
        await runeStatus(apps, 'Open');
        await activate(apps.swift);
        await scroll(center(await find(apps.swift, 'id=IssueRow', 2)));
        await activate(apps.gpui);
        await scroll(gpui.row(apps, 3));
        await activate(apps.rune);
        await scroll(center(await find(apps.rune, 'id=list-scroll')));
        await activate(apps.web);
        await web.toList(apps);
        await scroll(center(await find(apps.web, web.row(RENAMED))));
        await activate(apps.sim);
        await phone.toList(apps);
        await phone.scroll(apps);
      },
    },
  ];

  return {
    steps,

    /**
     * Each app at its open issues, at their top, before a take, and the
     * issues of earlier takes gone, so that each app opens this one's.
     */
    async reset(apps: Apps, sql: (query: string) => string) {
      sql(`with gone as (delete from issue where title like '${TITLE}%'
          returning id),
        c as (delete from comment where "issueID" in (select id from gone)),
        l as (delete from "issueLabel" where "issueID" in (select id from gone))
        delete from emoji where "subjectID" in (select id from gone)`);
      await sleep(1500);
      // No dialog or menu left open by a take that stopped: GPUI's edit
      // dialog keeps Escape for its fields, so its close button.
      await activate(apps.rune);
      await key('esc');
      const cancel = tryFind(apps.rune, 'id=issue-sheet-dialog')
        ? tryFind(apps.rune, 'id=cancel')
        : undefined;
      if (cancel) await click(center(cancel));
      await activate(apps.gpui);
      await click(gpui.closeDialog(apps));
      await activate(apps.rune);
      await runeStatus(apps, 'Open');
      // The phone at its list: from the simulator's filters, to the
      // issues; from an issue, or one an earlier take deleted, back. The
      // simulator builds its tree once asked, so it can take a moment to
      // show either.
      await activate(apps.sim);
      await phone.toList(apps);
      // At its top, where the new issue lands.
      await phone.toTop(apps);
      // The web app at its list too.
      await activate(apps.web);
      await key('esc');
      await web.toList(apps);
      await activate(apps.swift);
      await click(center(await find(apps.swift, 'id=IssueRow', 0)));
    },

    /** What Postgres must have after the steps: failures, if any. */
    check(sql: (query: string) => string): string[] {
      const issue = sql(`select id from issue where title = '${RENAMED}'
        order by created desc limit 1`);
      if (!issue) return [`no issue titled "${RENAMED}"`];
      const failures: string[] = [];
      const expect = (what: string, got: string, want: string) => {
        if (got !== want) {
          failures.push(`${what}: got '${got}', want '${want}'`);
        }
      };
      expect(
        'open again',
        sql(`select open from issue where id = '${issue}'`),
        't',
      );
      expect(
        'description',
        sql(`select description from issue where id = '${issue}'`),
        DESCRIPTION,
      );
      expect(
        'comments',
        sql(`select string_agg(body, ' | ' order by created) from comment
          where "issueID" = '${issue}'`),
        `${IPHONE_COMMENT} | ${MAC_COMMENT} | ${WEB_COMMENT}`,
      );
      expect(
        'labels',
        sql(`select string_agg(l.name, ',') from "issueLabel" il
          join label l on l.id = il."labelID" where il."issueID" = '${issue}'`),
        LABEL,
      );
      return failures;
    },
  };
}
