#!/bin/sh
# Cargo's runner on macOS (.cargo/config.toml): runs zbugs-gpui from inside
# an app bundle, `ZBugs GPUI.app` beside the executable, so it launches as
# an app with its icon (AppIcon.icon) in the Dock and the app switcher. The
# bundle stays, to open from Finder. Tests and other executables run as
# they are.
set -eu
exe=$1
shift
[ "$(basename "$exe")" = zbugs-gpui ] || exec "$exe" "$@"

here=$(cd "$(dirname "$0")" && pwd)
# Absolute: actool takes relative paths from a directory of its own.
app="$(cd "$(dirname "$exe")" && pwd)/ZBugs GPUI.app"
res="$app/Contents/Resources"
mkdir -p "$app/Contents/MacOS" "$res"

# The icon, through Xcode's actool, when it changed: Assets.car for macOS 26
# and later, AppIcon.icns for earlier ones.
if [ ! -e "$res/Assets.car" ] || [ -n "$(find "$here/AppIcon.icon" -newer "$res/Assets.car")" ]; then
  partial=$(mktemp)
  out=$(xcrun actool "$here/AppIcon.icon" --compile "$res" --platform macosx \
    --minimum-deployment-target 11.0 --app-icon AppIcon \
    --output-partial-info-plist "$partial" --output-format human-readable-text) ||
    { echo "$out" >&2; exit 1; }
  rm -f "$partial"
fi

cat > "$app/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>zbugs-gpui</string>
	<key>CFBundleIdentifier</key>
	<string>dev.rocicorp.zbugs-gpui</string>
	<key>CFBundleName</key>
	<string>ZBugs GPUI</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIconName</key>
	<string>AppIcon</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF

# A hard link, not a copy: the executable can be large in a debug build.
ln -f "$exe" "$app/Contents/MacOS/zbugs-gpui"
# LaunchServices can see the bundle before it's whole, as when actool runs,
# and the Dock then shows the app as broken: register it as it is now.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
  -f "$app"
exec "$app/Contents/MacOS/zbugs-gpui" "$@"
