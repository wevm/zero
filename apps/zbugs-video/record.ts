/**
 * Records the four zbugs apps side by side on one screen, against one
 * backend: zbugs-swift on macOS, zbugs-gpui, zbugs-rune, and zbugs-swift on
 * an iPhone simulator. One cursor and keyboard rotate the storyboard's steps
 * (storyboard.ts) across the apps, so each change shows up live in the other
 * three. Captions for the steps go on afterwards; then Postgres must have
 * each change.
 *
 *   node apps/zbugs-video/record.ts [build|seed|launch|record|all]
 *
 * macOS only. Needs Xcode, Rust, Node 24, Homebrew's ffmpeg, cliclick,
 * xcodegen, and psql, the backend running (apps/zbugs-swift/server/server.ts),
 * its crew token in ZBUGS_TOKEN, and the Accessibility and Screen Recording
 * permissions for the app that runs this (see README.md).
 *
 * Environment:
 *   ZBUGS_TOKEN        the backend's crew token
 *   ZBUGS_DB_URL       the backend's database
 *                      (default postgresql://postgres@127.0.0.1:5432/zbugs_swift)
 *   ZBUGS_CACHE_URL    zero-cache, for the apps (default http://127.0.0.1:4848)
 *   ZBUGS_API_URL      the API server (default http://127.0.0.1:5173)
 *   ZBUGS_VIDEO_DIR    builds, the apps' local stores, and the videos
 *                      (default ~/.zbugs-video)
 *   ZBUGS_VIDEO_SIM    the simulator to use (default iPhone 17)
 *   ZBUGS_VIDEO_DISPLAY  `keep` to leave the display's mode alone; by default,
 *                      a screen too small for the four windows switches to
 *                      its largest HiDPI mode for the recording (displayplacer),
 *                      then back
 */
import {execFileSync, spawn, type ChildProcess} from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import {homedir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {storyboard, TITLE, type Apps} from './storyboard.ts';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '../..');
const env = process.env;
const work = env['ZBUGS_VIDEO_DIR'] ?? join(homedir(), '.zbugs-video');
const dbURL =
  env['ZBUGS_DB_URL'] ?? 'postgresql://postgres@127.0.0.1:5432/zbugs_swift';
const ax = join(work, 'ax');
const derived = join(work, 'xcode');
const swiftApp = join(derived, 'Build/Products/Debug/ZBugs.app');
const iosApp = join(derived, 'Build/Products/Debug-iphonesimulator/ZBugs.app');
// Each app's smallest window: zbugs-rune's is 900x520.
const MIN_CELL = {w: 900, h: 520};
const LARGE_LIST = 20_000;
// What `displayplacer list` and ffmpeg print.
const HIDPI_MODE = /res:(\d+)x(\d+) hz:(\d+).*scaling:on/g;
const CURRENT_MODE =
  /res:\d+x\d+ hz:\d+ color_depth:\d+( scaling:on)? <-- current/;
const SCREEN_DEVICE = /\[(\d+)\] Capture screen 0/;
const VIDEO_SIZE = /Video: .*?, (\d+)x\d+/;

function run(cmd: string, args: string[], opts: {cwd?: string} = {}): string {
  return execFileSync(cmd, args, {
    cwd: opts.cwd ?? repo,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
    maxBuffer: 64 << 20,
  }).trim();
}

function say(line: string) {
  process.stdout.write(`${line}\n`);
}

function step(what: string) {
  say(`==> ${what}`);
}

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));

function sql(query: string): string {
  return run('psql', [dbURL, '-v', 'ON_ERROR_STOP=1', '-qAtc', query]);
}

// --- build ---

function build() {
  mkdirSync(work, {recursive: true});
  step('ax, the accessibility helper');
  run('swiftc', ['-O', join(here, 'ax.swift'), '-o', ax]);
  for (const app of ['zbugs-rune', 'zbugs-gpui']) {
    step(app);
    execFileSync('cargo', ['build', '--release', '--locked'], {
      cwd: join(repo, 'apps', app),
      stdio: 'inherit',
    });
  }
  step('zbugs-swift for macOS and the iOS simulator');
  const project = join(repo, 'apps/zbugs-swift/App');
  run('xcodegen', ['generate'], {cwd: project});
  for (const [destination, ...settings] of [
    ['platform=macOS', 'CODE_SIGN_STYLE=Manual', 'CODE_SIGN_IDENTITY=-'],
    [`platform=iOS Simulator,id=${simulator()}`, 'CODE_SIGNING_ALLOWED=NO'],
  ]) {
    execFileSync(
      'xcodebuild',
      [
        'build',
        '-quiet',
        '-project',
        'ZBugs.xcodeproj',
        '-scheme',
        'ZBugsApp',
        '-destination',
        destination,
        '-derivedDataPath',
        derived,
        '-skipPackagePluginValidation',
        ...settings,
      ],
      {cwd: project, stdio: 'inherit'},
    );
  }
}

// --- seed ---

/**
 * Adds about 20k issues to the zero project, from zbugs' synthetic data
 * generator, for the fast scroll. Once: it skips a database that has them.
 */
function seed() {
  const count = Number(sql('select count(*) from issue'));
  if (count >= LARGE_LIST) {
    step(`seed: the database has ${count} issues already`);
    return;
  }
  step(`seed: ${LARGE_LIST} synthetic issues`);
  const out = join(work, 'synthetic');
  execFileSync(process.execPath, ['scripts/generate-synthetic.ts'], {
    cwd: join(repo, 'apps/zbugs'),
    stdio: ['ignore', 'ignore', 'inherit'],
    env: {
      ...env,
      ['NUM_ISSUES']: String(LARGE_LIST),
      ['NUM_PROJECTS']: '1',
      ['NUM_USERS']: '10',
      ['COMMENTS_PER_ISSUE']: '0',
      ['OUTPUT_DIR']: out,
    },
  });
  // Without triggers, so the issues keep the generator's dates instead of
  // all being modified now. Their creators are the seed's users.
  execFileSync('psql', [dbURL, '-v', 'ON_ERROR_STOP=1', '-q', '-f', '-'], {
    input: `
set session_replication_role = replica;
create temp table synthetic (id text, title text, open boolean,
  modified float8, created float8, "creatorID" text, "assigneeID" text,
  description text, visibility text, "projectID" text);
\\copy synthetic from '${join(out, 'issue_000.csv')}' csv header
insert into issue (id, title, open, modified, created, "creatorID",
  description, visibility, "projectID")
select s.id, s.title, s.open, s.modified, s.created,
  u.ids[1 + (row_number() over () % array_length(u.ids, 1))::int],
  s.description, 'public', p.id
from synthetic s,
  (select array_agg(id order by id) ids from "user") u,
  (select id from project where "lowerCaseName" = 'zero') p
on conflict do nothing;
`,
    stdio: ['pipe', 'inherit', 'inherit'],
  });
}

// --- launch ---

type Rect = {x: number; y: number; w: number; h: number};

function rect(s: string): Rect {
  const [x, y, w, h] = s.split(' ').map(Number);
  return {x, y, w, h};
}

function simulator(): string {
  const name = env['ZBUGS_VIDEO_SIM'] ?? 'iPhone 17';
  const devices = JSON.parse(
    run('xcrun', ['simctl', 'list', 'devices', 'available', '-j']),
  ).devices as Record<string, {udid: string; name: string}[]>;
  // The newest iOS runtime's.
  for (const runtime of Object.keys(devices).sort().reverse()) {
    if (!runtime.includes('iOS')) continue;
    const d = devices[runtime].find(d => d.name === name);
    if (d) return d.udid;
  }
  throw new Error(`no available simulator named ${name}`);
}

function simulatorApp(): string {
  const developer = run('xcode-select', ['-p']);
  const bundled = join(developer, 'Applications/Simulator.app');
  if (existsSync(bundled)) return bundled;
  // An Xcode beta can come without it: any installed Xcode's runs the
  // same runtimes.
  const found = run('mdfind', [
    'kMDItemCFBundleIdentifier == com.apple.iphonesimulator',
  ]).split('\n')[0];
  if (!found) throw new Error('Simulator.app is not installed');
  return found;
}

const pidsFile = () => join(work, 'pids.json');

function appEnv(): NodeJS.ProcessEnv {
  if (!env['ZBUGS_TOKEN']) throw new Error('ZBUGS_TOKEN is the crew token');
  return {...env};
}

function background(
  cmd: string,
  args: string[],
  opts: {cwd?: string; env?: NodeJS.ProcessEnv; log: string},
): ChildProcess {
  const log = openSync(join(work, opts.log), 'w');
  const child = spawn(cmd, args, {
    cwd: opts.cwd,
    env: opts.env,
    detached: true,
    stdio: ['ignore', log, log],
  });
  child.unref();
  return child;
}

async function waitFor<T>(what: string, f: () => T | undefined): Promise<T> {
  const deadline = Date.now() + 60_000;
  for (;;) {
    try {
      const v = f();
      if (v !== undefined) return v;
    } catch {
      // Not yet.
    }
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(500);
  }
}

function quit() {
  if (!existsSync(pidsFile())) return;
  const pids = JSON.parse(readFileSync(pidsFile(), 'utf8')) as Apps;
  for (const pid of [pids.rune, pids.gpui, pids.swift]) {
    try {
      process.kill(pid);
    } catch {
      // Gone already.
    }
  }
}

async function launch(): Promise<Apps> {
  quit();
  const e = appEnv();
  step('zbugs-rune and zbugs-gpui');
  const rune = background('./bundle.sh', ['target/release/zbugs-rune'], {
    cwd: join(repo, 'apps/zbugs-rune'),
    env: {...e, ['ZBUGS_RUNE_DIR']: join(work, 'rune')},
    log: 'rune.log',
  });
  const gpui = background('./bundle.sh', ['target/release/zbugs-gpui'], {
    cwd: join(repo, 'apps/zbugs-gpui'),
    env: {...e, ['ZBUGS_APP_DIR']: join(work, 'gpui')},
    log: 'gpui.log',
  });
  step('zbugs-swift on macOS');
  const swift = background(join(swiftApp, 'Contents/MacOS/ZBugs'), [], {
    env: e,
    log: 'swift.log',
  });
  step('zbugs-swift on the simulator');
  const udid = simulator();
  try {
    run('xcrun', ['simctl', 'boot', udid]);
  } catch {
    // Booted already.
  }
  run('open', ['-a', simulatorApp()]);
  run('xcrun', ['simctl', 'bootstatus', udid, '-b']);
  run('xcrun', ['simctl', 'install', udid, iosApp]);
  const simEnv: NodeJS.ProcessEnv = {...process.env};
  for (const k of ['ZBUGS_TOKEN', 'ZBUGS_CACHE_URL', 'ZBUGS_API_URL']) {
    if (e[k]) simEnv[`SIMCTL_CHILD_${k}`] = e[k];
  }
  execFileSync(
    'xcrun',
    [
      'simctl',
      'launch',
      '--terminate-running-process',
      udid,
      'dev.rocicorp.zbugs',
    ],
    {env: simEnv, stdio: 'ignore'},
  );
  const sim = Number(run('pgrep', ['-x', 'Simulator']));
  const apps: Apps = {
    rune: rune.pid!,
    gpui: gpui.pid!,
    swift: swift.pid!,
    sim,
  };
  writeFileSync(pidsFile(), JSON.stringify(apps));
  for (const [name, pid] of Object.entries(apps)) {
    await waitFor(`${name}'s window`, () => run(ax, ['window', String(pid)]));
  }
  return apps;
}

// --- layout ---

function displays(): {id: string; current: string; best: string} | undefined {
  let list: string;
  try {
    list = run('displayplacer', ['list']);
  } catch {
    return undefined;
  }
  const main = list.split('Persistent screen id: ')[1];
  if (!main) return undefined;
  const id = main.split('\n')[0].trim();
  const modes = [...main.matchAll(HIDPI_MODE)];
  const current = CURRENT_MODE.exec(main)?.[0].replace(' <-- current', '');
  if (!current || modes.length === 0) return undefined;
  const [best] = modes.sort(
    (a, b) => Number(b[1]) * Number(b[2]) - Number(a[1]) * Number(a[2]),
  );
  return {
    id,
    current,
    best: `res:${best[1]}x${best[2]} hz:${best[3]} scaling:on`,
  };
}

function setMode(id: string, mode: string) {
  run('displayplacer', [`id:${id} ${mode} enabled:true origin:(0,0) degree:0`]);
}

/** The display's mode before `layout` changed it, to go back to. */
const savedMode = () => join(work, 'display-mode');

function restoreDisplay() {
  if (!existsSync(savedMode())) return;
  const [id, ...mode] = readFileSync(savedMode(), 'utf8').split(' ');
  step(`display: back to ${mode.join(' ')}`);
  setMode(id, mode.join(' '));
  rmSync(savedMode());
}

/**
 * Fits the four windows on the main screen, two by two: Swift, GPUI, then
 * Rune, and the simulator in the fourth cell, at its height. A screen
 * smaller than four of the smallest Rune window switches to its largest
 * HiDPI mode first, until `restoreDisplay`.
 */
async function layout(apps: Apps) {
  let screen = rect(run(ax, ['screen']));
  if (
    (screen.w / 2 < MIN_CELL.w || screen.h / 2 < MIN_CELL.h) &&
    env['ZBUGS_VIDEO_DISPLAY'] !== 'keep'
  ) {
    const d = displays();
    if (d && d.best !== d.current) {
      step(`display: ${d.best}, from ${d.current}`);
      if (!existsSync(savedMode())) {
        writeFileSync(savedMode(), `${d.id} ${d.current}`);
      }
      setMode(d.id, d.best);
      await sleep(3000);
      screen = rect(run(ax, ['screen']));
    }
  }
  const w = Math.floor(screen.w / 2);
  const h = Math.floor(screen.h / 2);
  step(`layout: ${screen.w}x${screen.h} points, cells ${w}x${h}`);
  if (w < MIN_CELL.w || h < MIN_CELL.h) {
    say(
      `    the cells are smaller than zbugs-rune's ${MIN_CELL.w}x${MIN_CELL.h}: windows overlap`,
    );
  }
  const place = (pid: number, x: number, y: number, pw: number, ph: number) =>
    rect(
      run(ax, [
        'place',
        String(pid),
        String(screen.x + x),
        String(screen.y + y),
        String(pw),
        String(ph),
      ]),
    );
  place(apps.swift, 0, 0, w, h);
  place(apps.gpui, w, 0, w, h);
  place(apps.rune, 0, h, w, h);
  // The simulator without its bezel, as tall as the cell: it keeps the
  // phone's aspect, so it's narrower than asked for.
  bezels(false);
  const sim = place(apps.sim, w, h, w, h);
  place(apps.sim, w + Math.floor((w - sim.w) / 2), h, sim.w, sim.h);
}

function bezels(show: boolean) {
  run('osascript', [
    '-e',
    `tell application "System Events" to tell process "Simulator"
      set item_ to menu item "Show Device Bezels" of menu "Window" of menu bar 1
      if ((value of attribute "AXMenuItemMarkChar" of item_) is not missing value) is not ${show} then click item_
    end tell`,
  ]);
}

// --- record ---

/** The screen's index among ffmpeg's AVFoundation devices. */
function screenDevice(): string {
  let out = '';
  try {
    execFileSync(
      'ffmpeg',
      ['-hide_banner', '-f', 'avfoundation', '-list_devices', 'true', '-i', ''],
      {encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']},
    );
  } catch (e) {
    out = String((e as {stderr?: string}).stderr ?? '');
  }
  const m = SCREEN_DEVICE.exec(out);
  if (!m) throw new Error(`no screen among ffmpeg's devices:\n${out}`);
  return m[1];
}

type Caption = {start: number; end: number; title: string; detail: string};

async function record(apps: Apps) {
  await layout(apps);
  try {
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
    const raw = join(work, `raw-${stamp}.mp4`);
    const out = join(work, `zbugs-four-apps-${stamp}.mp4`);
    const screen = rect(run(ax, ['screen']));
    const board = storyboard(ax);

    // Each app at its list before the take, the simulator's keyboard hidden.
    await board.reset(apps, sql);

    step(`recording to ${raw}`);
    const ffmpeg = spawn(
      'ffmpeg',
      [
        '-v',
        'error',
        '-y',
        '-f',
        'avfoundation',
        '-framerate',
        '60',
        '-capture_cursor',
        '1',
        '-capture_mouse_clicks',
        '1',
        '-pixel_format',
        'nv12',
        '-i',
        `${screenDevice()}:none`,
        // VideoToolbox encodes up to 4096 wide; a Retina screen is wider.
        '-vf',
        "scale='min(3840,iw)':-2",
        '-c:v',
        'h264_videotoolbox',
        '-b:v',
        '40M',
        '-pix_fmt',
        'yuv420p',
        raw,
      ],
      {stdio: ['pipe', 'inherit', 'inherit']},
    );
    await waitFor('the recording to start', () =>
      existsSync(raw) && statSync(raw).size > 0 ? true : undefined,
    );
    const t0 = Date.now();
    const captions: Caption[] = [];
    try {
      await sleep(1500);
      for (const [i, s] of board.steps.entries()) {
        step(`${i + 1}/${board.steps.length} ${s.title}`);
        const start = (Date.now() - t0) / 1000;
        await s.run(apps);
        captions.push({
          start,
          end: (Date.now() - t0) / 1000,
          title: `${i + 1}/${board.steps.length}  ${s.title}`,
          detail: s.detail,
        });
      }
      await sleep(2000);
    } finally {
      ffmpeg.stdin.write('q');
      await new Promise(r => ffmpeg.on('exit', r));
    }
    captioned(raw, out, captions, screen, apps);
    step(`video: ${out}`);
  } finally {
    restoreDisplay();
  }
}

/**
 * Re-encodes the take at up to 3840 wide with each step's caption in the
 * fourth cell, beside the simulator.
 */
function captioned(
  raw: string,
  out: string,
  captions: Caption[],
  screen: Rect,
  apps: Apps,
) {
  step('captions');
  let probe = '';
  try {
    execFileSync('ffmpeg', ['-hide_banner', '-i', raw], {
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch (e) {
    probe = String((e as {stderr?: Buffer}).stderr ?? '');
  }
  const capturedWidth = Number(VIDEO_SIZE.exec(probe)?.[1]);
  if (!capturedWidth) throw new Error(`can't read the take's size:\n${probe}`);
  const outWidth = Math.min(3840, capturedWidth);
  // Points of the screen to pixels of the output: the full screen is
  // captured, menu bar and all.
  const full = rect(run(ax, ['screen']));
  const pointsWide = full.x + full.w;
  const k = outWidth / pointsWide;
  const sim = rect(run(ax, ['window', String(apps.sim)]));
  const cell = {
    x: screen.x + Math.floor(screen.w / 2),
    y: screen.y + Math.floor(screen.h / 2),
  };
  const margin = 24;
  const x = Math.round((cell.x + margin) * k);
  const width = Math.round((sim.x - cell.x - 2 * margin) * k);
  const y = Math.round((cell.y + margin * 2) * k);
  const font = existsSync('/System/Library/Fonts/SFNS.ttf')
    ? '/System/Library/Fonts/SFNS.ttf'
    : '/System/Library/Fonts/Helvetica.ttc';
  const big = Math.round(Math.min(width / 12, 34 * k));
  const small = Math.round(big * 0.62);
  const dir = join(work, 'captions');
  mkdirSync(dir, {recursive: true});
  const filters = captions.flatMap((c, i) => {
    const title = join(dir, `${i}-title.txt`);
    const detail = join(dir, `${i}-detail.txt`);
    writeFileSync(title, c.title);
    writeFileSync(detail, wrap(c.detail, Math.floor(width / (small * 0.5))));
    // Each caption stays up until the next one starts.
    const end = captions[i + 1]?.start ?? 1e9;
    const enable = `enable='between(t,${c.start.toFixed(2)},${end.toFixed(2)})'`;
    // A long title gets smaller to fit beside the phone.
    const size = Math.min(big, Math.floor(width / (0.55 * c.title.length)));
    return [
      `drawtext=fontfile=${font}:textfile=${title}:fontsize=${size}:fontcolor=white:x=${x}:y=${y}:${enable}`,
      `drawtext=fontfile=${font}:textfile=${detail}:fontsize=${small}:fontcolor=white@0.85:line_spacing=${Math.round(small * 0.35)}:x=${x}:y=${y + Math.round(big * 1.6)}:${enable}`,
    ];
  });
  execFileSync(
    'ffmpeg',
    [
      '-v',
      'error',
      '-y',
      '-i',
      raw,
      '-vf',
      [`scale=${outWidth}:-2`, ...filters].join(','),
      '-c:v',
      'libx264',
      '-crf',
      '20',
      '-preset',
      'medium',
      '-pix_fmt',
      'yuv420p',
      '-movflags',
      '+faststart',
      out,
    ],
    {stdio: 'inherit'},
  );
}

function wrap(text: string, width: number): string {
  const lines: string[] = [];
  let line = '';
  for (const word of text.split(' ')) {
    if (line && (line + ' ' + word).length > width) {
      lines.push(line);
      line = word;
    } else {
      line = line ? `${line} ${word}` : word;
    }
  }
  return [...lines, line].join('\n');
}

// --- check ---

/** What the storyboard did, as Postgres has it. */
function check() {
  step('checking Postgres');
  const failures = storyboard(ax).check(sql);
  for (const f of failures) {
    say(`FAILED: ${f}`);
  }
  if (failures.length) process.exit(1);
  say(`ok: "${TITLE}" has each change`);
}

// --- main ---

const command = process.argv[2] ?? 'all';
switch (command) {
  case 'build':
    build();
    break;
  case 'seed':
    seed();
    break;
  case 'launch':
    await layout(await launch());
    break;
  case 'record': {
    const apps = existsSync(pidsFile())
      ? (JSON.parse(readFileSync(pidsFile(), 'utf8')) as Apps)
      : await launch();
    await record(apps);
    check();
    break;
  }
  case 'all':
    build();
    seed();
    await record(await launch());
    check();
    break;
  case 'quit':
    quit();
    restoreDisplay();
    break;
  default:
    process.stderr.write(
      'usage: node apps/zbugs-video/record.ts [build|seed|launch|record|all|quit]\n',
    );
    process.exit(2);
}
