#!/bin/sh
# Check a built Runberth.app: version, Sparkle settings, embedded framework,
# and signatures. Run by CI after build-macos-app.sh.
# Usage: check-macos-app.sh [APP]  (default: dist/Runberth.app)
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
app=${1:-$root/dist/Runberth.app}
plist="$app/Contents/Info.plist"
failed=0
fail() {
    echo "error: $*" >&2
    failed=1
}
value() {
    /usr/libexec/PlistBuddy -c "Print :$1" "$plist" 2>/dev/null || true
}

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
# Sparkle compares CFBundleVersion, so both must follow Cargo.toml.
for key in CFBundleShortVersionString CFBundleVersion; do
    [ "$(value $key)" = "$version" ] || fail "$key is '$(value $key)', expected $version"
done
# Info.plist is preprocessed, where an unguarded "//" would start a comment.
feed=https://github.com/vantalter/runberth/releases/latest/download/appcast.xml
[ "$(value SUFeedURL)" = "$feed" ] || fail "SUFeedURL is '$(value SUFeedURL)'"
key=$(value SUPublicEDKey)
if [ -z "$key" ] || [ "$key" != "$(cat "$root/app/macos/sparkle-public-key" 2>/dev/null)" ]; then
    fail "SUPublicEDKey does not match app/macos/sparkle-public-key"
fi
[ "$(value SUEnableAutomaticChecks)" = false ] || fail "SUEnableAutomaticChecks is not false"

sparkle="$app/Contents/Frameworks/Sparkle.framework"
[ -f "$sparkle/Versions/B/Sparkle" ] || fail "Sparkle.framework is not embedded"
[ ! -e "$sparkle/Versions/B/XPCServices" ] || fail "Sparkle's XPC services were not removed"
otool -L "$app/Contents/MacOS/runberth" | grep -q '@rpath/Sparkle.framework' ||
    fail "the executable does not link Sparkle"
otool -l "$app/Contents/MacOS/runberth" | grep -q '@executable_path/../Frameworks' ||
    fail "the executable cannot find embedded frameworks"
codesign --verify --deep --strict "$app" || fail "the app's signatures do not verify"

[ "$failed" = 0 ] && echo "$app: version $version, Sparkle, and signatures OK"
exit "$failed"
