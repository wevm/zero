//! Where `ZeroStorage::Sqlite { directory: None }` keeps its databases: a
//! folder of the app's own, named for its bundle identifier or executable,
//! by zero-swift's rules. Every app used to share one folder, where one
//! app's database GC and `drop_all_databases` deleted the others'
//! databases; the first open of a new folder moves its database from the
//! shared one (`SqliteBackend`).
//!
//! The rules take the environment, the executable's path, and a file
//! reader as arguments, so every platform's are tested on any.

use super::storage::{StorageFailure, StorageFailureKind};
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The platforms whose default folders differ.
#[derive(Clone, Copy, Debug)]
enum Platform {
    /// macOS and Mac Catalyst: the app's home is the user's, unless the app
    /// is sandboxed.
    MacOs,
    /// iOS, tvOS, watchOS, and visionOS: the app's home is its container.
    Container,
    Windows,
    /// Linux and the rest.
    Other,
}

impl Platform {
    fn current() -> Platform {
        if cfg!(any(
            target_os = "macos",
            all(target_os = "ios", target_abi = "macabi")
        )) {
            Platform::MacOs
        } else if cfg!(target_vendor = "apple") {
            Platform::Container
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// The default storage's folder, and the one every app shared before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DefaultDirectories {
    /// The app's own folder; `shared` for an app without a name to give it.
    pub(super) directory: PathBuf,
    /// The folder every app shared.
    pub(super) shared: PathBuf,
}

thread_local! {
    static FOR_TESTING: RefCell<Option<DefaultDirectories>> = const { RefCell::new(None) };
}

/// For tests: `ZeroStorage::Sqlite { directory: None }` on this thread keeps
/// its databases in `directory`, and moves them from `shared`, in place of
/// the platform's folders.
#[doc(hidden)]
pub fn set_default_sqlite_directories(directory: PathBuf, shared: PathBuf) {
    FOR_TESTING.with(|t| *t.borrow_mut() = Some(DefaultDirectories { directory, shared }));
}

/// The default folders of this app.
pub(super) fn default_directories() -> Result<DefaultDirectories, StorageFailure> {
    if let Some(d) = FOR_TESTING.with(|t| t.borrow().clone()) {
        return Ok(d);
    }
    // Resolved, so an app run through a symlink gets its own folder.
    let executable = std::env::current_exe()
        .ok()
        .map(|e| std::fs::canonicalize(&e).unwrap_or(e));
    resolve(
        Platform::current(),
        &|name| std::env::var_os(name),
        executable.as_deref(),
        &|path| std::fs::read(path).ok(),
    )
}

/// The default folders on `platform`, given the environment `var`, the
/// app's executable, and `read`, which reads a file:
///
/// - macOS and Mac Catalyst: `$HOME/Library/Application Support/<app>/Zero`,
///   where `<app>` is the bundle identifier of an executable in an app
///   bundle, or else the executable's name. A sandboxed app keeps the
///   shared folder, in a home that is its own container.
/// - iOS and Apple's other platforms: the shared folder,
///   `$XDG_DATA_HOME/zero` or else `$HOME/.local/share/zero`, in a home
///   that is the app's container.
/// - Windows: `%LOCALAPPDATA%\<app>\Zero`, `<app>` the executable's name
///   without its extension.
/// - Elsewhere: `$XDG_DATA_HOME/<app>/zero`, or else
///   `$HOME/.local/share/<app>/zero`, `<app>` the executable's name without
///   the ` (deleted)` Linux adds once the file is replaced, as an upgrade
///   does while the app runs.
///
/// The shared folder is the same without `<app>`; an app without an
/// `<app>` that is one path component keeps it.
fn resolve(
    platform: Platform,
    var: &dyn Fn(&str) -> Option<OsString>,
    executable: Option<&Path>,
    read: &dyn Fn(&Path) -> Option<Vec<u8>>,
) -> Result<DefaultDirectories, StorageFailure> {
    let var = |name: &str| var(name).filter(|v| !v.is_empty());
    let not_set = |name: &str| {
        StorageFailure::new(StorageFailureKind::CannotOpen, format!("{name} is not set"))
    };
    let home = || var("HOME").ok_or_else(|| not_set("HOME"));
    let xdg_data = || match var("XDG_DATA_HOME") {
        Some(data) => Ok(PathBuf::from(data)),
        None => Ok(Path::new(&home()?).join(".local/share")),
    };
    let (data, folder, app) = match platform {
        Platform::MacOs => (
            Path::new(&home()?).join("Library/Application Support"),
            "Zero",
            // Realm detects the sandbox this way.
            if var("APP_SANDBOX_CONTAINER_ID").is_some() {
                None
            } else {
                executable.and_then(|e| mac_app_name(e, read))
            },
        ),
        Platform::Windows => (
            PathBuf::from(var("LOCALAPPDATA").ok_or_else(|| not_set("LOCALAPPDATA"))?),
            "Zero",
            executable.and_then(Path::file_stem).map(OsStr::to_owned),
        ),
        Platform::Container => (xdg_data()?, "zero", None),
        Platform::Other => (
            xdg_data()?,
            "zero",
            executable.and_then(Path::file_name).map(|name| {
                let replaced = name.to_str().and_then(|n| n.strip_suffix(" (deleted)"));
                replaced.map_or(name, OsStr::new).to_owned()
            }),
        ),
    };
    let shared = data.join(folder);
    let directory = match app {
        Some(app) if is_path_component(&app) => data.join(app).join(folder),
        _ => shared.clone(),
    };
    Ok(DefaultDirectories { directory, shared })
}

/// Whether `name` is one entry of a folder: not empty, `.`, or `..`, and
/// without a separator or NUL.
fn is_path_component(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    !matches!(bytes, b"" | b"." | b"..") && !bytes.iter().any(|b| matches!(b, b'/' | b'\\' | 0))
}

/// The bundle identifier of an executable at `<X>.app/Contents/MacOS/`,
/// from `<X>.app/Contents/Info.plist`; else, or when that doesn't parse,
/// the executable's name.
fn mac_app_name(executable: &Path, read: &dyn Fn(&Path) -> Option<Vec<u8>>) -> Option<OsString> {
    let bundle_identifier = || {
        let macos = executable.parent()?;
        let contents = macos.parent()?;
        let bundle = contents.parent()?;
        if macos.file_name()? != "MacOS"
            || contents.file_name()? != "Contents"
            || bundle.extension()? != "app"
        {
            return None;
        }
        plist_bundle_identifier(&read(&contents.join("Info.plist"))?)
    };
    bundle_identifier()
        .map(OsString::from)
        .or_else(|| executable.file_name().map(OsStr::to_owned))
}

/// The top-level dictionary's `CFBundleIdentifier` string of a property
/// list, binary or XML; `None` when it doesn't parse.
fn plist_bundle_identifier(plist: &[u8]) -> Option<String> {
    if plist.starts_with(b"bplist00") {
        binary_bundle_identifier(plist)
    } else {
        xml_bundle_identifier(std::str::from_utf8(plist).ok()?)
    }
}

const KEY: &str = "CFBundleIdentifier";

/// A binary property list's: a trailer locates the offset table, which
/// locates each object; the top object is a dictionary, its keys' and
/// values' object references in two runs.
fn binary_bundle_identifier(b: &[u8]) -> Option<String> {
    let trailer = b.get(b.len().checked_sub(32)?..)?;
    let offset_size = usize::from(trailer[6]);
    let ref_size = usize::from(trailer[7]);
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) {
        return None;
    }
    let objects = uint(&trailer[8..16])?;
    let table = uint(&trailer[24..32])?;
    let object = |r: usize| {
        if r >= objects {
            return None;
        }
        let at = table.checked_add(r.checked_mul(offset_size)?)?;
        uint(b.get(at..at.checked_add(offset_size)?)?)
    };
    let (count, refs) = binary_object(b, object(uint(&trailer[16..24])?)?, 0xD)?;
    let run = count.checked_mul(ref_size)?;
    let keys = b.get(refs..refs.checked_add(run)?)?;
    let values = b.get(refs.checked_add(run)?..refs.checked_add(run.checked_mul(2)?)?)?;
    for (key, value) in keys
        .chunks_exact(ref_size)
        .zip(values.chunks_exact(ref_size))
    {
        if binary_string(b, object(uint(key)?)?).as_deref() == Some(KEY) {
            return binary_string(b, object(uint(value)?)?);
        }
    }
    None
}

/// A big-endian unsigned integer of 1 to 8 bytes.
fn uint(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    usize::try_from(bytes.iter().fold(0u64, |n, b| n << 8 | u64::from(*b))).ok()
}

/// The length of the object of `kind` (its marker's high nibble) at `at`,
/// and where its contents start. A length of 15 or more follows the
/// marker as an integer object: `0x1n`, then 2^n bytes.
fn binary_object(b: &[u8], at: usize, kind: u8) -> Option<(usize, usize)> {
    let marker = *b.get(at)?;
    if marker >> 4 != kind {
        return None;
    }
    if marker & 0xF != 0xF {
        return Some((usize::from(marker & 0xF), at + 1));
    }
    let int = *b.get(at + 1)?;
    if int >> 4 != 0x1 || int & 0xF > 3 {
        return None;
    }
    let size = 1 << (int & 0xF);
    let start = at + 2;
    Some((uint(b.get(start..start.checked_add(size)?)?)?, start + size))
}

/// The string at `at`: ASCII (`0x5_`) or UTF-16BE (`0x6_`, its length in
/// code units).
fn binary_string(b: &[u8], at: usize) -> Option<String> {
    if let Some((len, start)) = binary_object(b, at, 0x5) {
        let s = b.get(start..start.checked_add(len)?)?;
        return s
            .is_ascii()
            .then(|| String::from_utf8_lossy(s).into_owned());
    }
    let (len, start) = binary_object(b, at, 0x6)?;
    let s = b.get(start..start.checked_add(len.checked_mul(2)?)?)?;
    char::decode_utf16(s.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)))
        .collect::<Result<String, _>>()
        .ok()
}

/// An XML property list's: the `<string>` after the
/// `<key>CFBundleIdentifier</key>` of the top-level `<dict>`.
fn xml_bundle_identifier(xml: &str) -> Option<String> {
    // Of `<dict>`s and `<array>`s: a key at depth 1 is the top-level
    // dictionary's.
    let mut depth = 0usize;
    let mut rest = xml;
    loop {
        rest = &rest[rest.find('<')?..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = &comment[comment.find("-->")? + 3..];
            continue;
        }
        let end = rest.find('>')?;
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        match tag {
            "dict" | "array" => depth += 1,
            "/dict" | "/array" => depth = depth.checked_sub(1)?,
            "key" if depth == 1 => {
                let (key, after) = xml_text(rest, "</key>")?;
                rest = after;
                if key == KEY {
                    let value = rest.trim_start();
                    if value.starts_with("<string/>") {
                        return Some(String::new());
                    }
                    return Some(xml_text(value.strip_prefix("<string>")?, "</string>")?.0);
                }
            }
            _ => {}
        }
    }
}

/// The text of `s` before `end`, its entities decoded, and what follows
/// `end`. `None` for text holding markup, such as CDATA, or another entity.
fn xml_text<'a>(s: &'a str, end: &str) -> Option<(String, &'a str)> {
    let i = s.find(end)?;
    let mut text = &s[..i];
    if text.contains('<') {
        return None;
    }
    let mut out = String::new();
    while let Some(amp) = text.find('&') {
        out.push_str(&text[..amp]);
        text = &text[amp..];
        let (entity, c) = [
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&amp;", '&'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find(|(e, _)| text.starts_with(e))?;
        out.push(c);
        text = &text[entity.len()..];
    }
    out.push_str(text);
    Some((out, &s[i + end.len()..]))
}

/// Excludes the default folder from backups (Time Machine) once per
/// process, on a thread of its own: `tmutil` takes about a second. It sets
/// a sticky attribute on the folder, which some file operations clear, so
/// every launch sets it again (as Firestore does on every open). Failures
/// are ignored.
#[cfg(any(target_os = "macos", all(target_os = "ios", target_abi = "macabi")))]
pub(super) fn exclude_from_backups(directory: &Path) {
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, OnceLock};
    static EXCLUDED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    let first = EXCLUDED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(directory.to_path_buf());
    if !first {
        return;
    }
    let directory = directory.to_path_buf();
    std::thread::spawn(move || {
        let _ = Command::new("/usr/bin/tmutil")
            .arg("addexclusion")
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // From
    // `python3 -c 'import plistlib,sys; sys.stdout.buffer.write(plistlib.dumps({...}, fmt=plistlib.FMT_BINARY))'`
    // with `{"AAANested": {"CFBundleIdentifier": "wrong.nested"},
    // "CFBundleExecutable": "Notes", "CFBundleIdentifier": "com.example.notes",
    // "CFBundleName": "Notes"}`: a nested dictionary's identifier comes first.
    const BINARY: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd4, 0x01, 0x02, 0x03, 0x04, 0x05, 0x07,
        0x08, 0x07, 0x59, 0x41, 0x41, 0x41, 0x4e, 0x65, 0x73, 0x74, 0x65, 0x64, 0x5f, 0x10, 0x12,
        0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x45, 0x78, 0x65, 0x63, 0x75, 0x74, 0x61,
        0x62, 0x6c, 0x65, 0x5f, 0x10, 0x12, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x49,
        0x64, 0x65, 0x6e, 0x74, 0x69, 0x66, 0x69, 0x65, 0x72, 0x5c, 0x43, 0x46, 0x42, 0x75, 0x6e,
        0x64, 0x6c, 0x65, 0x4e, 0x61, 0x6d, 0x65, 0xd1, 0x03, 0x06, 0x5c, 0x77, 0x72, 0x6f, 0x6e,
        0x67, 0x2e, 0x6e, 0x65, 0x73, 0x74, 0x65, 0x64, 0x55, 0x4e, 0x6f, 0x74, 0x65, 0x73, 0x5f,
        0x10, 0x11, 0x63, 0x6f, 0x6d, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x2e, 0x6e,
        0x6f, 0x74, 0x65, 0x73, 0x08, 0x11, 0x1b, 0x30, 0x45, 0x52, 0x55, 0x62, 0x68, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7c,
    ];
    /// `{"CFBundleIdentifier": "com.example.café", "CFBundleName": "Café"}`:
    /// UTF-16 strings.
    const BINARY_UTF16: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd2, 0x01, 0x02, 0x03, 0x04, 0x5f, 0x10,
        0x12, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x49, 0x64, 0x65, 0x6e, 0x74, 0x69,
        0x66, 0x69, 0x65, 0x72, 0x5c, 0x43, 0x46, 0x42, 0x75, 0x6e, 0x64, 0x6c, 0x65, 0x4e, 0x61,
        0x6d, 0x65, 0x6f, 0x10, 0x10, 0x00, 0x63, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x2e, 0x00, 0x65,
        0x00, 0x78, 0x00, 0x61, 0x00, 0x6d, 0x00, 0x70, 0x00, 0x6c, 0x00, 0x65, 0x00, 0x2e, 0x00,
        0x63, 0x00, 0x61, 0x00, 0x66, 0x00, 0xe9, 0x64, 0x00, 0x43, 0x00, 0x61, 0x00, 0x66, 0x00,
        0xe9, 0x08, 0x0d, 0x22, 0x2f, 0x52, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5b,
    ];
    /// `{"CFBundleName": "Notes"}`.
    const BINARY_NO_ID: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd1, 0x01, 0x02, 0x5c, 0x43, 0x46, 0x42,
        0x75, 0x6e, 0x64, 0x6c, 0x65, 0x4e, 0x61, 0x6d, 0x65, 0x55, 0x4e, 0x6f, 0x74, 0x65, 0x73,
        0x08, 0x0b, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x1e,
    ];
    /// The first, from `fmt=plistlib.FMT_XML`.
    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AAANested</key>
	<dict>
		<key>CFBundleIdentifier</key>
		<string>wrong.nested</string>
	</dict>
	<key>CFBundleExecutable</key>
	<string>Notes</string>
	<key>CFBundleIdentifier</key>
	<string>com.example.notes</string>
	<key>CFBundleName</key>
	<string>Notes</string>
</dict>
</plist>
"#;

    const BUNDLE: &str = "/Applications/Notes.app/Contents/MacOS/Notes";
    const MAC_HOME: &str = "/Users/u";

    type Folders = Result<(String, String), StorageFailure>;

    /// The folders, with the variables `vars` set, the executable at `exe`,
    /// and `plist` in every file; and the files read.
    fn resolved(
        platform: Platform,
        vars: &[(&str, &str)],
        exe: Option<&str>,
        plist: Option<&[u8]>,
    ) -> (Folders, Vec<PathBuf>) {
        let reads = RefCell::new(Vec::new());
        let var = |name: &str| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        };
        let read = |path: &Path| {
            reads.borrow_mut().push(path.to_owned());
            plist.map(<[u8]>::to_vec)
        };
        let d = resolve(platform, &var, exe.map(Path::new), &read);
        let s = |p: PathBuf| p.into_os_string().into_string().unwrap();
        (d.map(|d| (s(d.directory), s(d.shared))), reads.into_inner())
    }

    fn folders(
        platform: Platform,
        vars: &[(&str, &str)],
        exe: Option<&str>,
        plist: Option<&[u8]>,
    ) -> Folders {
        resolved(platform, vars, exe, plist).0
    }

    fn mac(exe: Option<&str>, plist: Option<&[u8]>) -> (String, String) {
        folders(Platform::MacOs, &[("HOME", MAC_HOME)], exe, plist).unwrap()
    }

    const MAC_SHARED: &str = "/Users/u/Library/Application Support/Zero";

    #[test]
    fn an_app_bundle_on_macos_gets_a_folder_named_for_its_bundle_identifier() {
        let notes = "/Users/u/Library/Application Support/com.example.notes/Zero".to_owned();
        let shared = MAC_SHARED.to_owned();
        assert_eq!(
            mac(Some(BUNDLE), Some(XML.as_bytes())),
            (notes.clone(), shared.clone())
        );
        assert_eq!(mac(Some(BUNDLE), Some(BINARY)), (notes, shared.clone()));
        assert_eq!(
            mac(Some(BUNDLE), Some(BINARY_UTF16)),
            (
                "/Users/u/Library/Application Support/com.example.café/Zero".to_owned(),
                shared
            )
        );
        // An empty APP_SANDBOX_CONTAINER_ID isn't a sandbox.
        let unsandboxed = folders(
            Platform::MacOs,
            &[("HOME", MAC_HOME), ("APP_SANDBOX_CONTAINER_ID", "")],
            Some(BUNDLE),
            Some(BINARY),
        );
        assert_eq!(unsandboxed.unwrap(), mac(Some(BUNDLE), Some(BINARY)));
        let reads = resolved(Platform::MacOs, &[("HOME", MAC_HOME)], Some(BUNDLE), None).1;
        assert_eq!(
            reads,
            [Path::new("/Applications/Notes.app/Contents/Info.plist")]
        );
    }

    #[test]
    fn an_executable_on_macos_without_a_bundle_identifier_gets_a_folder_named_for_it() {
        let notes = (
            "/Users/u/Library/Application Support/Notes/Zero".to_owned(),
            MAC_SHARED.to_owned(),
        );
        // Not in a bundle, though every file holds BINARY.
        let cli = "/usr/local/bin/notes-cli";
        assert_eq!(
            mac(Some(cli), Some(BINARY)).0,
            "/Users/u/Library/Application Support/notes-cli/Zero"
        );
        for exe in [
            "/Applications/Notes/Contents/MacOS/Notes",
            "/Applications/Notes.app/Contents/Notes",
            "/Applications/Notes.app/Contents/Resources/Notes",
            "/Applications/Notes.app/MacOS/Notes",
        ] {
            assert_eq!(mac(Some(exe), Some(BINARY)), notes, "{exe}");
        }
        // In a bundle whose Info.plist is missing, has no identifier, or
        // doesn't parse.
        assert_eq!(mac(Some(BUNDLE), None), notes);
        assert_eq!(mac(Some(BUNDLE), Some(BINARY_NO_ID)), notes);
        assert_eq!(mac(Some(BUNDLE), Some(&BINARY[..BINARY.len() - 1])), notes);
        assert_eq!(mac(Some(BUNDLE), Some(b"bplist00")), notes);
        assert_eq!(
            mac(Some(BUNDLE), Some(b"<plist><dict></dict></plist>")),
            notes
        );
        assert_eq!(mac(Some(BUNDLE), Some(&[0xff, 0xfe, 0x3c])), notes);
    }

    #[test]
    fn a_sandboxed_app_on_macos_keeps_the_folder_in_its_container() {
        let home = "/Users/u/Library/Containers/com.example.notes/Data";
        let d = folders(
            Platform::MacOs,
            &[
                ("HOME", home),
                ("APP_SANDBOX_CONTAINER_ID", "com.example.notes"),
            ],
            Some(BUNDLE),
            Some(BINARY),
        );
        let shared = format!("{home}/Library/Application Support/Zero");
        assert_eq!(d.unwrap(), (shared.clone(), shared));
    }

    #[test]
    fn ios_and_the_other_container_platforms_keep_the_folder() {
        let home = "/var/mobile/Containers/Data/Application/A1";
        let (d, reads) = resolved(
            Platform::Container,
            &[("HOME", home), ("APP_SANDBOX_CONTAINER_ID", "")],
            Some("/var/containers/Bundle/Application/B2/Notes.app/Notes"),
            Some(BINARY),
        );
        let shared = format!("{home}/.local/share/zero");
        assert_eq!(d.unwrap(), (shared.clone(), shared));
        assert!(reads.is_empty(), "{reads:?}");
        let d = folders(
            Platform::Container,
            &[("HOME", home), ("XDG_DATA_HOME", "/data")],
            Some(BUNDLE),
            Some(BINARY),
        );
        assert_eq!(
            d.unwrap(),
            ("/data/zero".to_owned(), "/data/zero".to_owned())
        );
        assert!(folders(Platform::Container, &[], Some(BUNDLE), None).is_err());
    }

    #[test]
    fn elsewhere_the_folder_is_named_for_the_executable() {
        let linux = |vars: &[(&str, &str)]| {
            folders(Platform::Other, vars, Some("/usr/bin/notes"), Some(BINARY)).unwrap()
        };
        assert_eq!(
            linux(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")]),
            ("/data/notes/zero".to_owned(), "/data/zero".to_owned())
        );
        let home = (
            "/home/u/.local/share/notes/zero".to_owned(),
            "/home/u/.local/share/zero".to_owned(),
        );
        assert_eq!(linux(&[("HOME", "/home/u")]), home);
        assert_eq!(linux(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "")]), home);
        // The binary replaced while the app runs, as an upgrade does.
        let replaced = |exe| folders(Platform::Other, &[("HOME", "/home/u")], Some(exe), None);
        assert_eq!(replaced("/usr/bin/notes (deleted)").unwrap(), home);
        assert_eq!(
            replaced("/usr/bin/notes (deleted)x").unwrap().0,
            "/home/u/.local/share/notes (deleted)x/zero"
        );
        let shared = replaced("/usr/bin/ (deleted)").unwrap();
        assert_eq!(shared.0, shared.1);
        // The bundle rule is macOS's.
        let bundled = folders(
            Platform::Other,
            &[("HOME", "/home/u")],
            Some(BUNDLE),
            Some(BINARY),
        );
        assert_eq!(bundled.unwrap().0, "/home/u/.local/share/Notes/zero");
        assert!(folders(Platform::Other, &[], Some("/usr/bin/notes"), None).is_err());
    }

    #[test]
    fn on_windows_the_folder_is_named_for_the_executable_without_exe() {
        // `/`, so the path splits as on Windows.
        let local = "C:/Users/u/AppData/Local";
        let d = folders(
            Platform::Windows,
            &[("LOCALAPPDATA", local), ("HOME", "/home/u")],
            Some("C:/Program Files/Notes/notes.exe"),
            None,
        );
        assert_eq!(
            d.unwrap(),
            (format!("{local}/notes/Zero"), format!("{local}/Zero"))
        );
        let no_data = folders(
            Platform::Windows,
            &[("HOME", "/home/u")],
            Some("C:/n.exe"),
            None,
        );
        assert_eq!(no_data.unwrap_err().message, "LOCALAPPDATA is not set");
    }

    #[test]
    fn an_app_without_a_name_for_a_folder_keeps_the_shared_one() {
        let shared = (MAC_SHARED.to_owned(), MAC_SHARED.to_owned());
        assert_eq!(mac(None, None), shared);
        for exe in ["/", "/usr/bin/..", "/usr/bin/a\\b", "/usr/bin/a\0b"] {
            assert_eq!(mac(Some(exe), None), shared, "{exe:?}");
            let linux = folders(Platform::Other, &[("HOME", "/h")], Some(exe), None).unwrap();
            assert_eq!(linux.0, linux.1, "{exe:?}");
        }
        // A bundle identifier that isn't one path component, rather than the
        // executable's name.
        for id in ["com/example", "..", "", "a\\b"] {
            let xml = format!("<dict><key>CFBundleIdentifier</key><string>{id}</string></dict>");
            assert_eq!(mac(Some(BUNDLE), Some(xml.as_bytes())), shared, "{id:?}");
        }
        let empty = "<dict><key>CFBundleIdentifier</key><string/></dict>";
        assert_eq!(mac(Some(BUNDLE), Some(empty.as_bytes())), shared);
    }

    #[test]
    fn xml_identifiers_decode_the_five_entities() {
        let xml = "<plist><dict><!-- <key>CFBundleIdentifier</key><string>no</string> -->
            <array><dict><key>CFBundleIdentifier</key><string>nested</string></dict></array>
            <key>CFBundleIdentifier</key>
            <string>a&lt;b&gt;c&amp;d&quot;e&apos;f</string></dict></plist>";
        assert_eq!(
            plist_bundle_identifier(xml.as_bytes()).as_deref(),
            Some("a<b>c&d\"e'f")
        );
        for bad in [
            "<dict><key>CFBundleIdentifier</key><string>a&nbsp;b</string></dict>",
            "<dict><key>CFBundleIdentifier</key><string><![CDATA[a]]></string></dict>",
            "<dict><key>CFBundleIdentifier</key><integer>1</integer></dict>",
            "<array><dict><key>CFBundleIdentifier</key><string>a</string></dict></array>",
            "<dict><key>CFBundleIdentifier</key><string>a",
        ] {
            assert_eq!(plist_bundle_identifier(bad.as_bytes()), None, "{bad}");
        }
    }

    /// The parser never panics, whatever it reads: every prefix of each
    /// fixture, and each with any byte replaced.
    #[test]
    fn plists_that_are_cut_short_or_damaged_do_not_panic() {
        for plist in [BINARY, BINARY_UTF16, BINARY_NO_ID, XML.as_bytes()] {
            for end in 0..plist.len() {
                plist_bundle_identifier(&plist[..end]);
            }
            for i in 0..plist.len() {
                for byte in [
                    0x00, 0x01, 0x0f, 0x1f, 0x5f, 0x6f, 0xd1, 0xdf, 0xff, b'<', b'>',
                ] {
                    let mut damaged = plist.to_vec();
                    damaged[i] = byte;
                    plist_bundle_identifier(&damaged);
                }
            }
        }
    }
}
